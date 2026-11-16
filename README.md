# All In One (prototype 0.0.1)

Native Rust desktop app: visual workflows, local and cloud LLMs, notes, a browser tab, and a right-hand agent panel.
No Chromium or Electron. UI is egui (glow renderer) + egui-snarl for the node canvas.

## Run

Needs Rust 1.92+ (https://rustup.rs). Linux also needs the usual X11/Wayland + GL libs.

    cargo run -p aio-app --release

Workspace data lives in `~/AllInOne` (Windows: `%USERPROFILE%\AllInOne`). Override with `AIO_HOME=/some/path`.

    workflows/*.aio.json   one file per workflow (source of truth)
    notes/**/*.md          notes, folders are notebooks
    workflow-files/        the only place workflow file nodes can read and write

For local models install Ollama (https://ollama.com), then `ollama pull llama3.2:3b` (the Models tab shows the
recommended model for your hardware and copies the pull command). "Demo" provider works with no model at all.

## Test

    cargo test --workspace      # 23 tests

## What works

- Workflows: node canvas (right-click to add nodes), save/load, run with live per-node status and streamed output.
  Nodes: Text, Read file, Template, AI chat, Contains (true/false branches), Write file, Output.
- AI nodes use the Recommended model when the model field is empty; any model can be typed instead.
- Privacy: cloud providers are blocked unless allowed globally (Models tab) AND per AI node. File nodes cannot leave `workflow-files/`.
- Workflows hotbar: Text, Read file, Template, AI chat, Contains, Write file, Output buttons always visible above the canvas.
- Notes: Markdown files, search, `[[links]]` + backlinks, Source/Split/Preview with synced scrolling, autosave.
  Formatting toolbar (Style/headings, bold, italic, strikethrough, underline, highlight, code, lists, checklist, quote,
  divider, links), Cmd/Ctrl+B and +I, document font (bundled Sans/Serif/Mono plus Georgia/Arial/Times/Verdana/Courier
  when installed) and size.
- Ollama: started automatically in the background on launch (and stopped on exit only if the app started it).
  Models can be installed in-app with a progress bar: one-click "Install recommended", per-model Install, or any name.
  Missing models trigger an Install prompt instead of a failed run.
- Browser: text-mode page fetch, "agent can see this page" switch, clip to note, open in system browser.
- Models: hardware profile, fit estimate, Recommended badge (never a gate), Ollama / OpenAI-compatible / Demo providers.
- Agent panel on the right of every tab, collapsible. It sees the current tab (workflow, note, page) and streams replies.

## Not yet (honest list)

- Agent is chat only. No tool use yet (creating/editing workflows and notes), no permission prompts.
- Browser is not Servo yet: no JS, no sign-ins. The Servo go/no-go spike is Phase 0.
- GPU memory is only auto-detected for NVIDIA via `nvidia-smi`; otherwise set it by hand on the Models tab.
- No cron/file triggers, no run history database, no Python code node.
- Settings (font, provider, chosen model) are not persisted between launches. Notes editor has no line numbers or find/replace.
- Apple Silicon GPU memory is estimated as two thirds of RAM; adjust on the Models tab if needed.

## Layout

    crates/aio-core   workflow model, engine, provider trait, hardware recommender, notes store (no UI deps)
    crates/aio-llm    Ollama and OpenAI-compatible providers (streaming)
    crates/aio-app    egui shell

Fonts: DejaVu (see crates/aio-app/assets/fonts/NOTICE.txt).

Dev helpers: `AIO_AUTOADD=1`, `AIO_AUTOINSTALL=1`, `AIO_START_TAB=notes|browser|models`, `AIO_PROVIDER=demo`, `AIO_AUTORUN=1`.


## Notes outline
The Notes sidebar has an OUTLINE section listing the page's headings (H1 to H3). Click one to jump to it in Preview, Split or Source view. The sidebar's lower block (outline, backlinks, links) is resizable.

## Updating
In the app: download the new `all-in-one-prototype.tar.gz` to `~/Downloads`, open the **Settings** tab (it shows a dot when a newer file is waiting) and press **Update now**. It rebuilds in the background, then **Restart now** launches the new version (macOS and Linux).

From a terminal, from the project folder:

    ./update.sh

It unpacks the newest tarball, replaces the source (keeping `target/` for fast rebuilds and leaving `~/AllInOne` alone), keeps the previous version in `all-in-one.prev`, then builds and runs. Use `./update.sh --no-run` to skip launching, or pass a path to use a specific tarball.

## Workflow nodes
Pick a group in the ADD NODE bar (Input, Tools, Scripts, AI, Output), or right-click the canvas.

| Group | Nodes |
|---|---|
| Input | Text, File in (txt/md/csv/json), Note in (notebook page), API get (URL, headers) |
| Tools | Template, Contains (true/false), Replace (regex optional), Filter lines, Lines (sort, unique, trim, number), JSON path, Count, Convert (CSV and JSON) |
| Scripts | Python (stdin in, stdout out, 60 s limit, needs `python3`), SQL (SQLite file, or incoming CSV/JSON as table `input`; `:input` binds the incoming text safely) |
| AI | AI chat, with a model dropdown: Auto (recommended), installed local models, Claude and OpenAI models |
| Output | Output, File out (text/Markdown/CSV/JSON, replace or append), Note out (replace or append), API send (POST/PUT/PATCH), Database out (insert rows into SQLite) |

File and database paths are relative to the workflow files folder (`~/AllInOne/workflow-files`) and cannot escape it.

## Cloud providers
Models tab, Cloud providers: paste a Claude (Anthropic) and/or OpenAI API key. The model list is fetched from the provider, and models can be added by ID. Cloud models appear in each AI node's dropdown and as the default model.

Privacy gates, both off by default (Models tab):
- *Allow workflows and the agent to send data to cloud providers*, plus *May use cloud models* on each AI node.
- *Allow API nodes to use the network* (API get / API send).

Keys are stored in `~/AllInOne/settings.json` (owner-only permissions on macOS/Linux), or read from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`. Proper OS keychain storage is a planned improvement.

## Releases and in-app updates
Releases are built by GitHub Actions (`.github/workflows/release.yml`) for macOS (Apple Silicon and Intel), Linux and Windows. Publish one with:

    scripts/release.sh 0.2.0

The Settings tab checks the repository's latest release (at startup and every six hours; switch it off under *Source and options*), downloads the build for your platform, verifies its SHA-256, swaps the binary in place (the old one is kept as `all-in-one.prev`) and offers a restart. A private repository needs a fine-grained read-only token (Settings, *Source and options*). Tags with a suffix such as `0.3.0-beta.1` are pre-releases and are only offered on the beta channel.

### First install from a release
Download `all-in-one-<your platform>.tar.gz` from the Releases page and unpack it. On macOS the browser marks downloads as quarantined, and these builds are not notarised yet, so run once:

    xattr -dr com.apple.quarantine all-in-one

Later updates happen inside the app and are not affected.

## Installing on macOS (no terminal)

Each release includes `All-In-One-aarch64.dmg` (Apple Silicon) and `All-In-One-x86_64.dmg` (Intel).
Open the disk image and drag **All In One** into **Applications**, then launch it from Launchpad or Spotlight.

The app is ad-hoc signed, not notarised (that needs a paid Apple Developer ID, about £79/year), so the first
launch of a downloaded copy is blocked by Gatekeeper. Right-click the app, choose **Open**, then **Open** again.
If macOS says it is damaged, run once: `xattr -cr "/Applications/All In One.app"`.

In-app updates keep working: they replace the binary inside the app and keep your data in `~/AllInOne`.
To build the disk image yourself: `scripts/make-app.sh`.
