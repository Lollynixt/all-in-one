//! Implementations of the non-AI node kinds. Everything here is free of UI and mostly synchronous so it is easy to test.
use crate::workflow::{ConvertOp, CountWhat, FileFormat, LineOp, MatchMode, SqlMode, Table};
use regex::Regex;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{params_from_iter, Connection};
use serde_json::{Map, Value as Json};
use std::path::Path;
use std::time::Duration;

const MAX_OUTPUT: usize = 5 * 1024 * 1024;
const MAX_ROWS: usize = 50_000;
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

// ------------------------------------------------------------------ text tools

pub fn replace(input: &str, find: &str, with: &str, regex: bool) -> Result<String, String> {
    if find.is_empty() {
        return Ok(input.to_string());
    }
    if regex {
        let re = Regex::new(find).map_err(|e| format!("bad regex: {e}"))?;
        Ok(re.replace_all(input, with).into_owned())
    } else {
        Ok(input.replace(find, with))
    }
}

pub fn filter_lines(input: &str, pattern: &str, mode: MatchMode, regex: bool) -> Result<String, String> {
    let re = if regex { Some(Regex::new(pattern).map_err(|e| format!("bad regex: {e}"))?) } else { None };
    let needle = pattern.to_lowercase();
    let hit = |l: &str| match &re {
        Some(re) => re.is_match(l),
        None => l.to_lowercase().contains(&needle),
    };
    let keep = mode == MatchMode::Keep;
    Ok(input.lines().filter(|l| hit(l) == keep).collect::<Vec<_>>().join("\n"))
}

pub fn lines_op(input: &str, op: LineOp) -> String {
    let mut lines: Vec<String> = input.lines().map(str::to_string).collect();
    match op {
        LineOp::SortAsc => lines.sort_by_key(|l| l.to_lowercase()),
        LineOp::SortDesc => {
            lines.sort_by_key(|l| l.to_lowercase());
            lines.reverse();
        }
        LineOp::Unique => {
            let mut seen = std::collections::HashSet::new();
            lines.retain(|l| seen.insert(l.clone()));
        }
        LineOp::Reverse => lines.reverse(),
        LineOp::Trim => lines.iter_mut().for_each(|l| *l = l.trim().to_string()),
        LineOp::DropEmpty => lines.retain(|l| !l.trim().is_empty()),
        LineOp::Number => lines = lines.into_iter().enumerate().map(|(i, l)| format!("{}. {l}", i + 1)).collect(),
    }
    lines.join("\n")
}

pub fn count(input: &str, what: CountWhat) -> String {
    match what {
        CountWhat::Lines => input.lines().count(),
        CountWhat::Words => input.split_whitespace().count(),
        CountWhat::Chars => input.chars().count(),
    }
    .to_string()
}

/// Walks `a.b.0.c` (also `$.a.b[0].c`) through JSON. Strings come out raw, everything else as pretty JSON.
pub fn json_path(input: &str, path: &str) -> Result<String, String> {
    let v: Json = serde_json::from_str(input).map_err(|e| format!("input is not valid JSON: {e}"))?;
    let norm = path.trim().trim_start_matches('$').replace('[', ".").replace(']', "");
    let mut cur = &v;
    for seg in norm.split('.').filter(|s| !s.is_empty()) {
        cur = match cur {
            Json::Array(a) => seg.parse::<usize>().ok().and_then(|i| a.get(i)),
            Json::Object(o) => o.get(seg),
            _ => None,
        }
        .ok_or_else(|| format!("path not found at '{seg}'"))?;
    }
    Ok(match cur {
        Json::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    })
}

// ------------------------------------------------------------------ CSV / JSON

pub fn parse_csv(s: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = vec![];
    let mut row: Vec<String> = vec![];
    let mut field = String::new();
    let mut in_quotes = false;
    let mut pending = false; // a field has started on this row
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if in_quotes {
            if c == '"' {
                if it.peek() == Some(&'"') {
                    field.push('"');
                    it.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                in_quotes = true;
                pending = true;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                pending = true;
            }
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                pending = false;
            }
            _ => {
                field.push(c);
                pending = true;
            }
        }
    }
    if pending || !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
    rows
}

pub fn to_csv(rows: &[Vec<String>]) -> String {
    let q = |f: &str| {
        if f.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", f.replace('"', "\"\""))
        } else {
            f.to_string()
        }
    };
    rows.iter().map(|r| r.iter().map(|f| q(f)).collect::<Vec<_>>().join(",")).collect::<Vec<_>>().join("\n")
}

fn infer(s: &str) -> Json {
    if let Ok(i) = s.parse::<i64>() {
        if i.to_string() == s {
            return Json::from(i);
        }
    }
    if let Ok(f) = s.parse::<f64>() {
        if f.is_finite() && f.to_string() == s {
            return Json::from(f);
        }
    }
    match s {
        "true" => Json::Bool(true),
        "false" => Json::Bool(false),
        _ => Json::String(s.to_string()),
    }
}

pub fn csv_to_json(input: &str) -> Result<String, String> {
    let rows = parse_csv(input);
    let Some((head, rest)) = rows.split_first() else { return Ok("[]".into()) };
    let out: Vec<Json> = rest
        .iter()
        .map(|r| {
            let mut o = Map::new();
            for (i, h) in head.iter().enumerate() {
                o.insert(h.clone(), r.get(i).map_or(Json::Null, |v| infer(v)));
            }
            Json::Object(o)
        })
        .collect();
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// Turns a JSON array of objects (or one object) into header + string rows.
fn json_rows(input: &str) -> Result<(Vec<String>, Vec<Vec<Json>>), String> {
    let v: Json = serde_json::from_str(input).map_err(|e| format!("input is not valid JSON: {e}"))?;
    let items = match v {
        Json::Array(a) => a,
        o @ Json::Object(_) => vec![o],
        _ => return Err("expected a JSON array of objects".into()),
    };
    let mut head: Vec<String> = vec![];
    for it in &items {
        let Json::Object(o) = it else { return Err("expected a JSON array of objects".into()) };
        for k in o.keys() {
            if !head.contains(k) {
                head.push(k.clone());
            }
        }
    }
    let rows = items.iter().map(|it| head.iter().map(|h| it.get(h).cloned().unwrap_or(Json::Null)).collect()).collect();
    Ok((head, rows))
}

fn json_cell(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn json_to_csv(input: &str) -> Result<String, String> {
    let (head, rows) = json_rows(input)?;
    let mut all = vec![head];
    all.extend(rows.iter().map(|r| r.iter().map(json_cell).collect()));
    Ok(to_csv(&all))
}

pub fn convert(input: &str, op: ConvertOp) -> Result<String, String> {
    match op {
        ConvertOp::CsvToJson => csv_to_json(input),
        ConvertOp::JsonToCsv => json_to_csv(input),
    }
}

/// Shapes text for a file of the given format. Text and Markdown are written as-is.
pub fn shape_for_file(input: &str, fmt: FileFormat) -> String {
    match fmt {
        FileFormat::Text | FileFormat::Markdown => input.to_string(),
        FileFormat::Json => {
            if let Ok(v) = serde_json::from_str::<Json>(input) {
                return serde_json::to_string_pretty(&v).unwrap_or_else(|_| input.to_string());
            }
            let rows = parse_csv(input);
            if rows.len() >= 2 && rows[0].len() >= 2 {
                if let Ok(j) = csv_to_json(input) {
                    return j;
                }
            }
            serde_json::to_string_pretty(&serde_json::json!({ "text": input })).unwrap_or_default()
        }
        FileFormat::Csv => json_to_csv(input).unwrap_or_else(|_| input.to_string()),
    }
}

/// Adds the format's extension when the path has none.
pub fn with_default_ext(path: &str, fmt: FileFormat) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.contains('.') {
        path.to_string()
    } else {
        format!("{path}.{}", fmt.ext())
    }
}

// ------------------------------------------------------------------ SQL

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn valid_table(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Rows from upstream text: JSON array of objects, otherwise CSV. First row is the header.
fn tabular(input: &str) -> Vec<Vec<Value>> {
    if let Ok((head, rows)) = json_rows(input) {
        let mut out = vec![head.into_iter().map(Value::Text).collect()];
        out.extend(rows.into_iter().map(|r| r.into_iter().map(json_to_sql).collect()));
        return out;
    }
    parse_csv(input).into_iter().map(|r| r.into_iter().map(Value::Text).collect()).collect()
}

fn json_to_sql(v: Json) -> Value {
    match v {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Integer(b as i64),
        Json::Number(n) => n.as_i64().map(Value::Integer).unwrap_or_else(|| Value::Real(n.as_f64().unwrap_or(0.0))),
        Json::String(s) => Value::Text(s),
        other => Value::Text(other.to_string()),
    }
}

fn header_names(row: &[Value]) -> Vec<String> {
    row.iter()
        .enumerate()
        .map(|(i, v)| match v {
            Value::Text(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => format!("col{}", i + 1),
        })
        .collect()
}

fn insert_rows(conn: &mut Connection, table: &str, rows: &[Vec<Value>]) -> Result<usize, String> {
    let Some((head, body)) = rows.split_first() else { return Err("nothing to insert: input has no rows".into()) };
    let cols = header_names(head);
    let col_defs = cols.iter().map(|c| quote_ident(c)).collect::<Vec<_>>().join(", ");
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(&format!("CREATE TABLE IF NOT EXISTS {} ({col_defs})", quote_ident(table)), []).map_err(|e| e.to_string())?;
    let marks = vec!["?"; cols.len()].join(", ");
    let sql = format!("INSERT INTO {} ({col_defs}) VALUES ({marks})", quote_ident(table));
    let mut n = 0;
    {
        let mut stmt = tx.prepare(&sql).map_err(|e| e.to_string())?;
        for r in body {
            let vals: Vec<Value> = (0..cols.len()).map(|i| r.get(i).cloned().unwrap_or(Value::Null)).collect();
            stmt.execute(params_from_iter(vals)).map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(n)
}

fn open_db(db: Option<&Path>) -> Result<Connection, String> {
    match db {
        Some(p) => {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            Connection::open(p).map_err(|e| format!("{}: {e}", p.display()))
        }
        None => Connection::open_in_memory().map_err(|e| e.to_string()),
    }
}

fn cell_text(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Null => String::new(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Blob(b) => format!("<blob {} bytes>", b.len()),
    }
}

fn cell_json(v: ValueRef<'_>) -> Json {
    match v {
        ValueRef::Null => Json::Null,
        ValueRef::Integer(i) => Json::from(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map_or(Json::Null, Json::Number),
        ValueRef::Text(t) => Json::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Json::String(format!("<blob {} bytes>", b.len())),
    }
}

/// Runs SQL. With no database file the query runs against a scratch in-memory database where upstream
/// CSV/JSON is available as table `input`. In Query mode `:input` binds the whole upstream text safely.
pub fn run_sql(db: Option<&Path>, query: &str, mode: SqlMode, format: Table, input: &str) -> Result<String, String> {
    if query.trim().is_empty() {
        return Err("SQL is empty".into());
    }
    let mut conn = open_db(db)?;
    if mode == SqlMode::Execute {
        if db.is_none() {
            return Err("running statements needs a database file".into());
        }
        conn.execute_batch(query).map_err(|e| e.to_string())?;
        return Ok(format!("OK: statements ran. {} row(s) changed by the last one.", conn.changes()));
    }
    if db.is_none() && !input.trim().is_empty() {
        let rows = tabular(input);
        if !rows.is_empty() {
            insert_rows(&mut conn, "input", &rows)?;
        }
    }
    let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
    if stmt.parameter_index(":input").map_err(|e| e.to_string())?.is_some() {
        stmt.raw_bind_parameter(stmt.parameter_index(":input").map_err(|e| e.to_string())?.expect("checked"), input).map_err(|e| e.to_string())?;
    }
    let names: Vec<String> = stmt.column_names().into_iter().map(str::to_string).collect();
    if names.is_empty() {
        let changed = stmt.raw_execute().map_err(|e| e.to_string())?;
        return Ok(format!("OK: {changed} row(s) changed."));
    }
    let mut rows = stmt.raw_query();
    let mut csv_rows = vec![names.clone()];
    let mut json_rows: Vec<Json> = vec![];
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        if csv_rows.len() > MAX_ROWS {
            return Err(format!("more than {MAX_ROWS} rows returned; add a LIMIT"));
        }
        match format {
            Table::Csv => csv_rows.push((0..names.len()).map(|i| r.get_ref(i).map(cell_text).unwrap_or_default()).collect()),
            Table::Json => {
                let mut o = Map::new();
                for (i, n) in names.iter().enumerate() {
                    o.insert(n.clone(), r.get_ref(i).map(cell_json).unwrap_or(Json::Null));
                }
                json_rows.push(Json::Object(o));
                csv_rows.push(vec![]); // counts toward the cap
            }
        }
    }
    Ok(match format {
        Table::Csv => to_csv(&csv_rows),
        Table::Json => serde_json::to_string_pretty(&json_rows).map_err(|e| e.to_string())?,
    })
}

pub fn db_insert(db: &Path, table: &str, input: &str) -> Result<String, String> {
    if !valid_table(table) {
        return Err("table name may only contain letters, digits and underscores".into());
    }
    let rows = tabular(input);
    let mut conn = open_db(Some(db))?;
    let n = insert_rows(&mut conn, table, &rows)?;
    Ok(format!("Inserted {n} row(s) into {table}"))
}

// ------------------------------------------------------------------ scripts

pub async fn run_python(code: &str, input: &str, cwd: &Path) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;
    if code.trim().is_empty() {
        return Err("Python script is empty".into());
    }
    let _ = tokio::fs::create_dir_all(cwd).await;
    let candidates: &[&str] = if cfg!(windows) { &["python", "py"] } else { &["python3", "python"] };
    let mut child = None;
    for exe in candidates {
        match Command::new(exe)
            .args(["-u", "-c", code])
            .current_dir(cwd)
            .env("PYTHONIOENCODING", "utf-8")
            .env("AIO_WORKSPACE", cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => {
                child = Some(c);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("could not start {exe}: {e}")),
        }
    }
    let mut child = child.ok_or("Python was not found. Install Python 3 and make sure `python3` is on your PATH.")?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let data = input.as_bytes().to_vec();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(&data).await; // the script may exit without reading everything
        drop(stdin);
    });
    let out = tokio::time::timeout(SCRIPT_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("script timed out after {} s and was stopped", SCRIPT_TIMEOUT.as_secs()))?
        .map_err(|e| e.to_string())?;
    let _ = writer.await;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.chars().rev().take(3000).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(format!("Python exited with {}:\n{}", out.status, tail.trim()));
    }
    if out.stdout.len() > MAX_OUTPUT {
        return Err("script printed more than 5 MB".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
}

// ------------------------------------------------------------------ http

fn header_map(headers: &str) -> Result<reqwest::header::HeaderMap, String> {
    use reqwest::header::{HeaderName, HeaderValue};
    let mut map = reqwest::header::HeaderMap::new();
    for line in headers.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (k, v) = line.split_once(':').ok_or_else(|| format!("header needs 'Name: value': {line}"))?;
        let name = HeaderName::from_bytes(k.trim().as_bytes()).map_err(|e| format!("bad header name '{}': {e}", k.trim()))?;
        let value = HeaderValue::from_str(v.trim()).map_err(|e| format!("bad header value for '{}': {e}", k.trim()))?;
        map.insert(name, value);
    }
    Ok(map)
}

fn check_url(url: &str) -> Result<(), String> {
    let u = url.trim().to_lowercase();
    if u.starts_with("http://") || u.starts_with("https://") {
        Ok(())
    } else {
        Err(format!("only http and https URLs are allowed: {url}"))
    }
}

async fn read_response(resp: reqwest::Response) -> Result<String, String> {
    let status = resp.status();
    let body = resp.bytes().await.map_err(|e| e.to_string())?;
    if body.len() > MAX_OUTPUT {
        return Err("response is larger than 5 MB".into());
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    if status.is_success() {
        Ok(text)
    } else {
        Err(format!("HTTP {status}: {}", text.chars().take(500).collect::<String>()))
    }
}

pub async fn http_get(url: &str, headers: &str) -> Result<String, String> {
    check_url(url)?;
    let client = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build().map_err(|e| e.to_string())?;
    let resp = client.get(url.trim()).headers(header_map(headers)?).send().await.map_err(|e| e.to_string())?;
    read_response(resp).await
}

pub async fn http_send(method: crate::workflow::HttpMethod, url: &str, headers: &str, body: &str) -> Result<String, String> {
    use crate::workflow::HttpMethod;
    check_url(url)?;
    let client = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build().map_err(|e| e.to_string())?;
    let m = match method {
        HttpMethod::Post => reqwest::Method::POST,
        HttpMethod::Put => reqwest::Method::PUT,
        HttpMethod::Patch => reqwest::Method::PATCH,
    };
    let mut headers = header_map(headers)?;
    if !headers.contains_key(reqwest::header::CONTENT_TYPE) {
        let ct = if serde_json::from_str::<Json>(body).is_ok() { "application/json" } else { "text/plain; charset=utf-8" };
        headers.insert(reqwest::header::CONTENT_TYPE, reqwest::header::HeaderValue::from_static(ct));
    }
    let resp = client.request(m, url.trim()).headers(headers).body(body.to_string()).send().await.map_err(|e| e.to_string())?;
    read_response(resp).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_round_trip_with_quotes() {
        let csv = "name,note\n\"Smith, J\",\"said \"\"hi\"\"\"\nBob,plain\r\n";
        let rows = parse_csv(csv);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], vec!["Smith, J", "said \"hi\""]);
        assert_eq!(parse_csv(&to_csv(&rows)), rows);
    }

    #[test]
    fn csv_json_conversions() {
        let j = csv_to_json("id,name,ok\n1,Ann,true\n007,Bo,false").unwrap();
        let v: Json = serde_json::from_str(&j).unwrap();
        assert_eq!(v[0]["id"], 1);
        assert_eq!(v[1]["id"], "007"); // leading zeros stay text
        assert_eq!(v[0]["ok"], true);
        let back = json_to_csv(&j).unwrap();
        assert!(back.starts_with("id,name,ok\n1,Ann,true"));
        assert!(json_to_csv("42").is_err());
    }

    #[test]
    fn text_tools() {
        assert_eq!(replace("a-b-c", "-", "+", false).unwrap(), "a+b+c");
        assert_eq!(replace("a1b22", r"\d+", "#", true).unwrap(), "a#b#");
        assert!(replace("x", "(", "", true).is_err());
        assert_eq!(filter_lines("Error x\nok\nerror y", "error", MatchMode::Keep, false).unwrap(), "Error x\nerror y");
        assert_eq!(filter_lines("a\nb", "a", MatchMode::Drop, false).unwrap(), "b");
        assert_eq!(lines_op("b\na\nb", LineOp::Unique), "b\na");
        assert_eq!(lines_op("b\nA\nc", LineOp::SortAsc), "A\nb\nc");
        assert_eq!(lines_op("x\ny", LineOp::Number), "1. x\n2. y");
        assert_eq!(count("one two\nthree", CountWhat::Words), "3");
    }

    #[test]
    fn json_paths() {
        let j = r#"{"a":{"b":[{"c":"hit"},{"c":2}]}}"#;
        assert_eq!(json_path(j, "a.b.0.c").unwrap(), "hit");
        assert_eq!(json_path(j, "$.a.b[1].c").unwrap(), "2");
        assert!(json_path(j, "a.x").is_err());
        assert!(json_path("nope", "a").is_err());
    }

    #[test]
    fn file_shaping() {
        assert_eq!(with_default_ext("out/report", FileFormat::Csv), "out/report.csv");
        assert_eq!(with_default_ext("out/report.txt", FileFormat::Csv), "out/report.txt");
        assert!(shape_for_file("a,b\n1,2", FileFormat::Json).contains("\"a\""));
        assert!(shape_for_file(r#"[{"a":1,"b":"x"}]"#, FileFormat::Csv).starts_with("a,b\n1,x"));
        assert!(shape_for_file("hello", FileFormat::Json).contains("\"text\""));
    }

    #[test]
    fn sql_over_input_and_param() {
        let csv = "team,pts\nred,3\nblue,5\nred,4";
        let out = run_sql(None, "SELECT team, SUM(CAST(pts AS INTEGER)) AS total FROM input GROUP BY team ORDER BY team", SqlMode::Query, Table::Csv, csv).unwrap();
        assert_eq!(out, "team,total\nblue,5\nred,7");
        let j = run_sql(None, "SELECT :input AS echoed", SqlMode::Query, Table::Json, "hi; DROP TABLE x").unwrap();
        assert!(j.contains("hi; DROP TABLE x"));
        assert!(run_sql(None, "SELEC nope", SqlMode::Query, Table::Csv, "").is_err());
        assert!(run_sql(None, "CREATE TABLE t(a)", SqlMode::Execute, Table::Csv, "").is_err());
    }

    #[test]
    fn database_insert_then_query() {
        let dir = std::env::temp_dir().join(format!("aio-db-{}", uuid::Uuid::new_v4()));
        let db = dir.join("t.db");
        let msg = db_insert(&db, "people", r#"[{"name":"Ann","age":31},{"name":"Bo","age":28}]"#).unwrap();
        assert!(msg.contains("2 row"));
        db_insert(&db, "people", "name,age\nCy,40").unwrap();
        let out = run_sql(Some(&db), "SELECT name FROM people WHERE age > 30 ORDER BY name", SqlMode::Query, Table::Csv, "").unwrap();
        assert_eq!(out, "name\nAnn\nCy");
        assert!(db_insert(&db, "bad name;", "a\n1").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn python_runs_stdin_to_stdout() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return; // no Python on this machine
        }
        let dir = std::env::temp_dir();
        let out = run_python("import sys\nprint(sys.stdin.read().upper())", "hello", &dir).await.unwrap();
        assert_eq!(out, "HELLO");
        let err = run_python("raise SystemExit('boom')", "", &dir).await.unwrap_err();
        assert!(err.contains("boom"));
    }

    #[test]
    fn url_checks() {
        assert!(check_url("https://example.com").is_ok());
        assert!(check_url("file:///etc/passwd").is_err());
        assert!(header_map("Authorization: Bearer x\n\nX-A: b").is_ok());
        assert!(header_map("nocolon").is_err());
    }
}
