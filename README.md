# All In One

A native desktop app that puts a visual workflow builder, local and cloud LLMs, Markdown notes, a built-in browser tab and an AI agent panel in one window. Written in Rust (egui), no Chromium or Electron.

Status: early prototype. Releases are tagged `v0.x`; expect rough edges and breaking changes.

## Install

### macOS
Download `All-In-One-aarch64.dmg` (Apple Silicon) or `All-In-One-x86_64.dmg` (Intel) from the [latest release](../../releases/latest), open it and drag **All In One** into **Applications**.

The app is ad-hoc signed but not notarised, so Gatekeeper blocks the first launch: right-click the app, choose **Open**, then **Open** again. If macOS reports the app as damaged, run once:

    xattr -cr "/Applications/All In One.app"

### Windows and Linux
Download `all-in-one-<target>.tar.gz` for your platform from the latest release and unpack it. Targets: `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`. Linux needs the usual X11/Wayland and OpenGL libraries.

### From source
Needs Rust 1.92+ ([rustup](https://rustup.rs)).

    cargo run -p aio-app --release

## Updating
Open **Settings → Updates**. The app checks this repository's latest GitHub release at startup and every six hours (switch off under *Source and options*), then **Update to vX.Y.Z** downloads the build for your platform, verifies its SHA-256, swaps the binary (the previous one is kept as `all-in-one.prev`) and offers **Restart now**. Your data is never touched.

Settings: repository to follow, optional token (only needed if the repository is private; a fine-grained read-only *Contents* token is enough), and a beta channel for pre-releases (tags such as `0.3.0-beta.1`).

Developer path: **Settings → Developer: update from a downloaded source file** and `./update.sh` rebuild from a source tarball.

## Data
Everything lives in `~/AllInOne` (Windows: `%USERPROFILE%\AllInOne`). Override with `AIO_HOME=/some/path`.

    workflows/*.aio.json   one file per workflow (source of truth)
    notes/**/*.md          notes; folders are notebooks
    workflow-files/        the only place file and database nodes can read or write
    settings.json          preferences and API keys (owner-only permissions on macOS/Linux)

## Features

**Workflows.** Node canvas (right-click or use the category bar to add nodes), save/load, run with live per-node status and streamed output.

| Group | Nodes |
|---|---|
| Input | Text, File in (txt/md/csv/json), Note in, API get (URL, headers) |
| Tools | Template, Contains (true/false), Replace (regex optional), Filter lines, Lines (sort, unique, trim, number), JSON path, Count, Convert (CSV/JSON) |
| Scripts | Python (stdin/stdout, 60 s limit, needs `python3`), SQL (SQLite file, or incoming CSV/JSON as table `input`; `:input` binds the incoming text safely) |
| AI | AI chat with a model dropdown: Auto, installed local models, Claude and OpenAI models |
| Output | Output, File out (text/Markdown/CSV/JSON, replace or append), Note out, API send (POST/PUT/PATCH), Database out (SQLite insert) |

File and database paths are relative to `workflow-files/` and cannot escape it. Click a File node to pick a file with the system file dialog.

**Notes.** Markdown files with search, `[[links]]` and backlinks, a heading outline with click-to-jump, Source/Split/Preview with synced scrolling, a formatting toolbar, selectable document font and size, autosave.

**Models.** Hardware profile and a recommended local model (a hint, never a gate). Ollama is started in the background and models can be installed in-app with a progress bar. Cloud providers: Anthropic (Claude) and OpenAI, plus any OpenAI-compatible endpoint. A Demo provider works with no model at all.

**Browser.** Text-mode page fetch, "agent can see this page" switch, clip to note, open in the system browser.

**Agent panel.** Collapsible, on every tab. It sees the current tab (workflow, note or page) and streams replies.

## Privacy
Off by default, enabled in the Models tab:
- Sending data to cloud providers needs the global switch **and** the per-node *May use cloud models* tick.
- API nodes need *Allow API nodes to use the network*.

Update checks contact `api.github.com` and send nothing about you. API keys are stored in `settings.json`, or read from `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` and `GITHUB_TOKEN`. OS keychain storage is planned.

## Not yet
- Agent is chat only: no tool use (editing workflows and notes) or permission prompts.
- Browser is text-mode: no JavaScript or sign-ins. A Servo-based engine is being evaluated.
- No schedules or file triggers, and no run-history database.
- GPU memory is auto-detected for NVIDIA only; Apple Silicon is estimated as two thirds of RAM (adjustable in the Models tab).
- macOS builds are not notarised; Windows and Linux builds are unsigned.

## Development

    cargo test --workspace

    crates/aio-core   workflow model, engine, node logic, notes store, provider trait, hardware recommender (no UI deps)
    crates/aio-llm    Ollama, OpenAI-compatible and Anthropic providers, model routing
    crates/aio-app    egui application

Releases are built by GitHub Actions (`.github/workflows/release.yml`) for macOS (Apple Silicon and Intel), Linux and Windows when a `v*` tag is pushed, or by hand from **Actions → Release → Run workflow** with an existing tag.

    scripts/release.sh 0.2.0         tag and push a release
    scripts/package-local.sh v0.2.0  build and package this machine's platform for the updater
    scripts/make-app.sh              build the macOS .app and .dmg

Developer environment variables: `AIO_HOME`, `AIO_PROVIDER=demo`, `AIO_START_TAB=notes|browser|models|settings`, `AIO_AUTORUN=1`, `AIO_LOAD=<workflow>`, `AIO_NO_UPDATE_CHECK=1`, `AIO_UPDATE_API=<url>`.

Fonts: DejaVu (see `crates/aio-app/assets/fonts/NOTICE.txt`).
