# rgg — design (v1)

A small, fast TUI for finding text in files and jumping straight to VS Code. Search only, no replace.

## Stack
- Rust, binary `rgg`. ratatui + crossterm. Windows first, nothing Windows-only.
- Search engine: ripgrep's crates linked in (`ignore`, `grep-searcher`, `grep-regex`). No `rg.exe` needed at runtime.
- Syntax highlighting: `syntect` (pure-Rust regex backend). Only the visible part of the preview is highlighted.

## Usage
`rgg [PATH] [-t TYPE]... [--regex]` searches PATH (default: current directory). CLI flags override the saved config for that run.

## Layout
- Upper left: search input, with the status line under it.
- Left, under the search: the result list.
- Right, full height: the file preview.
- Bottom: a footer with shortcuts. Entries drop out by priority when the terminal is narrow. F1 shows full help.

## Search
- Runs as you type, ~150 ms after the last keystroke, and cancels the previous run. Starts at 2 or more characters.
- Modes: literal (default) and regex.
- Case: smart (default), sensitive, or insensitive.
- Toggles: include hidden files and include ignored files (both off by default, like rg).
- File types: ripgrep's built-in type list plus `xaml` (`*.xaml`, `*.axaml`), which ripgrep lacks. You can pick several, e.g. `cs`, `xaml`. Own types go in the config under `[custom_types]` (name → globs) and add to or replace built-in ones; an invalid glob shows as an error in the status line.
- Limits (configurable): binary files skipped, files over 10 MB skipped, stop at 10,000 matching files ("file limit reached" shown).
- Fixed limit: at most 5,000 matches kept per file. Files over it show `(5000+)` and the status line reports "N truncated".
- A match is one occurrence: `foo foo` is two matches. Ctrl+N/P and Enter work per occurrence.
- Expressions: `a AND b`, `a OR b`, `NOT c`, in literal and regex mode (see below).
- Invalid regex: a red message in the status line. Any running search is cancelled, so it can't overwrite the error, but the rows already on screen stay.

### Expressions
- `AND`, `OR` and `NOT` are operators only as upper-case words with a space (or the start / end of the query) on both sides: `ANDROID`, `FOO_AND_BAR` or `xAND` are plain text. In regex mode an escaped space doesn't count, so `foo\ AND\ bar` searches for that text.
- The text between operators is one term, spaces included: `user name AND id`. Without operators the query is searched exactly as typed, so `foo bar` is still one phrase.
- Precedence NOT > AND > OR: `Error AND NOT Stack OR Warn` = `(Error AND (NOT Stack)) OR Warn`. `foo NOT bar` reads as `foo AND NOT bar`. A dangling operator while typing (`foo AND `) is ignored.
- Every OR branch needs a term to search for: `NOT foo` alone is an error.
- Smart case applies to each term separately: in `Error AND warn`, `Error` is case-sensitive and `warn` is not.
- Quotes are ordinary text, so `"Error"` finds the string literal. To find ` AND ` itself, use regex mode with `\ AND\ `.
- Scope, toggled with Ctrl+F and saved globally (default: line):
  - **line**: AND / NOT apply to one line; the results are the lines that satisfy the expression.
  - **file**: AND / NOT apply to the whole file; a file counts when it satisfies an OR branch, and its results are the lines containing a term of a satisfied branch. `IFoo AND NOT Dispose` = files using `IFoo` that never mention `Dispose`.
- Terms are highlighted; negated terms aren't. While an expression is active, the status line shows `[AND per line]` or `[AND per file]`.
- How it runs: each file is scanned once with a regex of all terms combined, as fast as a normal search; only the lines it finds are checked against the expression.

## Result list
- Default: one row per file, e.g. `path (N)`. Configurable (Ctrl+L or settings): one row per occurrence, e.g. `path:line:col  text`, with that occurrence highlighted strongly.
- Sorted by path while results stream in. The selection stays on the same occurrence, not the same row index. After a new search, the previous selection is selected again when it reappears, unless you have already picked something else.
- Paths are relative to the search root and shortened from the left (`…core/src/test.rs`), so the file name stays visible.
- Grouped by repository (setting "Group by repository", on by default). A file's repository is found the same way as for folder settings: the nearest folder above it containing `.git`, otherwise the search folder. When results come from more than one repository, each group gets a header row (`─ api ───`), and paths in the group are relative to that repository. ↑/↓ and clicks skip header rows. Within one repository the list looks exactly as without grouping.
- Tree view: later, not in v1.

## Preview
- The whole file, scrollable. Opens centred on the current match, with a line-number gutter.
- All matches have a light highlight. The current match has a strong one.
- Long lines are cut off by default. Alt+W toggles wrap. In wrap mode the preview scrolls by screen row, so every part of a very long line can be reached.
- Syntax highlighting never blocks input. Visible text is drawn right away, and colors catch up in short slices (about 8 ms each). Only the first 50,000 lines of a file are colored.
- Encoding: like rg. A UTF-16 BOM is detected, anything else is decoded lossily as UTF-8.

## Opening in the editor
- Enter runs `editor_command`. Default: `code {root} -g {file}:{line}:{col}`. `{col}` counts UTF-16 code units, like VS Code: in `😀foo`, `f` is column 3.
- `{root}` is the repository of the selected file (see grouping above). Without a git repo, it's the search path itself.
- The TUI stays open by default. Configurable: quit after opening.

## Keys (single focus: typing always goes to the search box)
| Key | Action |
|---|---|
| ↑ / ↓, PgUp / PgDn | move in the result list |
| Ctrl+Home / Ctrl+End | first / last result (see the VS Code note below) |
| Home / End, Ctrl+A / Ctrl+E | start / end of the search text |
| Ctrl+N / Ctrl+P | next/prev match in the selected file (per-file mode) |
| Ctrl+↑ / Ctrl+↓ | scroll the preview |
| Enter | open in VS Code at the current match |
| Ctrl+R | toggle regex |
| Ctrl+F | AND / NOT per line or per file |
| Ctrl+T | file type picker |
| Ctrl+O | settings |
| Alt+W | toggle preview wrap |
| Alt+P | hide / show the preview |
| Alt+← / Alt+→ | move the list/preview split by `resize_step` columns (default 2) |
| Alt+↑ / Alt+↓ | older / newer search from the history of this repository |
| Ctrl+Backspace | delete the previous word part in the search box: `getUserName` → `getUser`, `max_file` → `max_` (also Ctrl+W, Ctrl+H, Ctrl+_ for terminals that send those) |
| Ctrl+Delete | delete the next word part (also Alt+D) |
| Ctrl+← / Ctrl+→ | move the cursor by word part, same boundaries as Ctrl+Backspace (also Alt+B / Alt+F) |
| F1 | help overlay |
| Esc | close popup, otherwise quit |

In VS Code's integrated terminal, Ctrl+Home / Ctrl+End scroll the terminal and never reach rgg. To pass them through, add to VS Code's `settings.json`:

```json
"terminal.integrated.commandsToSkipShell": [
  "-workbench.action.terminal.scrollToTop",
  "-workbench.action.terminal.scrollToBottom"
]
```

Mouse: the wheel scrolls the pane under the cursor (list or preview). Clicking a row in the list selects it. Dragging the border between list and preview resizes them (both borders light up while hovered); double-clicking it resets the split to 40/60. The split and hidden preview last for the session only and are not saved. The preview's right border is a scrollbar: the thumb shows the visible part of the file, `■` marks every match (the current one in yellow), and clicking or dragging on it jumps there; the bottom border shows the visible line range.

## Config
`~/.rgg/config.toml` (on Windows: `%USERPROFILE%\.rgg\config.toml`). Written only when a setting actually changes, through a temp file and a rename, so the write is atomic. Nothing is ever written into the searched repositories.

If the file can't be read or parsed, rgg starts with defaults and never writes to it. Settings → "Config file" overwrites it, and only when you choose that.

CLI flags (`-t`, `--regex`) apply to that run only. Changing a per-folder setting saves only that setting, never a CLI override.

### Search history

A search is added to the history when Enter opens a result, and when rgg quits with a query that has results. Typing alone never adds anything, so partial queries stay out. Each repository (git root, same key as folder settings) has its own list, newest first; searching the same text again moves it to the top, and only `history_size` entries are kept. An entry stores the query and whether it was a regex.

Alt+↑ goes to older entries and Alt+↓ to newer ones; past the newest, the text you had typed comes back. A recalled query is searched at once, and a regex entry switches to regex mode for this session without changing the folder setting. Typing or editing ends browsing. History lives in `~/.rgg/history.toml`, separate from the config, and every write re-reads it so several rgg windows don't overwrite each other. Settings → "Clear history of this repository" empties the list.

### Per-folder settings
- Keyed by the git root (or the search path when there's no git repo). Stored in the same user config file, never in the repo.
- Per folder: `types`, `hidden`, `no_ignore`, `mode`. Everything else is global.
- Saved automatically: the last values used in a folder are remembered. A folder with no entry starts from the global defaults.
- The search text is not remembered. Each start begins with an empty search.
- Values that come from the folder entry are marked in the status line, e.g. `[cs]●`. Settings has a "Reset folder to defaults" entry.

```toml
mode = "literal"            # literal | regex
case = "smart"              # smart | sensitive | insensitive
types = ["cs"]
hidden = false
no_ignore = false
list_mode = "file"          # file | match
wrap = false
quit_after_open = false
max_file_size_mb = 10
max_files = 10000
min_query_len = 2
resize_step = 2             # columns per Alt+←/→
history_size = 100          # searches kept per repository, 0 = off
theme = "base16-ocean.dark"
editor_command = "code {root} -g {file}:{line}:{col}"

[custom_types]               # own file types, e.g. for -t / Ctrl+T
razor = ["*.razor", "*.cshtml"]

[folders.'D:\Foo']
types = ["cs"]
hidden = false
no_ignore = false
mode = "literal"

[folders.'D:\Bar']
types = ["py"]
mode = "regex"
```
