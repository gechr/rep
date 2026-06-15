# rep

`rep` is a fast find-and-replace tool, based on [fastmod](https://github.com/facebookincubator/fastmod).

Features plain and regex replacement, case-aware rewrites, interactive preview, line deletion, file listing, stdin mode, and multiple replacements in one pass.

By default `rep` prints a diff without touching the filesystem; pass `--write` to apply changes or `--preview` to step through them interactively.

## Install

### macOS / Linux

```shell
brew install gechr/tap/rep
```

### Windows

```shell
scoop bucket add gechr https://github.com/gechr/scoop-bucket
scoop install gechr/rep
```

### Cargo

```shell
cargo install --git https://github.com/gechr/rep
```

## Usage

<img src="assets/help.png" alt="help" width="700">

## Examples

```sh
# Replace "1.2.3" with "4.5.6" in all files
rep 1.2.3 4.5.6

# Replace "foo" with "bar" in "*.txt" files
rep -f txt foo bar

# Replace "foo" with "bar" in all (hidden) files
rep --hidden foo bar

# Replace "foo" with "bar" in all (hidden) Dockerfiles
rep -f '=Dockerfile' --hidden foo bar

# Replace "foo" with "bar" in all files and preview changes
rep --preview foo bar

# Replace "1.2.3" and "3.2.1" with "4.5.6" in all files
rep --regex '[13]\.2\.[13]' 4.5.6

# Swap "foo.bar" with "bar.foo" in all files
rep --regex '(foo)\.(bar)' '$2.$1'

# Replace "f.oo" and "F.OO" with "bar"
rep --ignore-case 'f.oo' bar

# Smart-replace in all files:
#  "foo_bar" with "hello_world"
#  "FooBar"  with "HelloWorld"
#  "FOO_BAR" with "HELLO_WORLD"
rep --smart foo_bar hello_world

# Read from stdin and replace "foo" with "bar"
echo foo bar | rep foo bar
rep foo bar < foobar.txt

# Apply multiple replacements in one pass
rep -e foo bar -e baz qux src

# Delete every line containing "TODO"
rep -d TODO
```

Run `rep --help` for the full reference, including the styling and hyperlink flags.

## Case-aware replacement

`rep` offers two strategies for case-aware replacement. They solve different problems and are mutually exclusive - passing both flags on the command line is an error. A value in `~/.config/rep/config.toml` or a `REP_*` env var can still be overridden by either flag on the command line.

### `-S, --smart` - identifier renames across naming conventions

Generates the standard identifier case variants of `<find>` and `<replace>` (snake_case, camelCase, PascalCase, kebab-case, SCREAMING_SNAKE_CASE, Train-Case, Ada_Case) and rewrites whichever variant appears in the source to the matching variant of the replacement.

```sh
rep --smart foo_bar hello_world
```

| Source    | Output        |
| --------- | ------------- |
| `foo_bar` | `hello_world` |
| `fooBar`  | `helloWorld`  |
| `FooBar`  | `HelloWorld`  |
| `FOO_BAR` | `HELLO_WORLD` |
| `foo-bar` | `hello-world` |

Use `--smart` when renaming an identifier across a codebase that uses several naming conventions for the same logical concept.

### `-P, --preserve` - mirror the source's letter case

Matches the pattern case-insensitively as a literal string, then projects the source's letter-case shape onto the replacement: `lowercase`, `Titlecase`, `UPPERCASE`. Anything else (mixed case) falls back to the replacement as authored.

```sh
rep --preserve colour color
```

| Source   | Output  | Shape               |
| -------- | ------- | ------------------- |
| `colour` | `color` | lowercase           |
| `Colour` | `Color` | Titlecase           |
| `COLOUR` | `COLOR` | UPPERCASE           |
| `cOlOuR` | `color` | mixed → as-authored |

Use `--preserve` for prose, docs, or string-literal rewrites where the same word appears in different case shapes and you want the replacement to follow each one.

## Configuration

`rep` reads defaults from `~/.config/rep/config.toml` (or `$XDG_CONFIG_HOME/rep/config.toml`). The file uses TOML; keys mirror the CLI flag names in kebab-case.

```toml
# ~/.config/rep/config.toml
hidden = true
ignore-case = true
preview-tool = "delta"
hyperlink-format = "vscode"
```

Precedence is **config file < environment variable < CLI flag**. Each configurable flag has a matching `REP_*` env var (e.g. `REP_HIDDEN`, `REP_PREVIEW_TOOL`), accepting `1/0/true/false/yes/no/on/off` for booleans.

Set `REP_CONFIG_PATH` to use a different file (e.g. for project-local config), or set it to an empty string to disable config loading entirely.

```sh
REP_CONFIG_PATH=./rep.toml rep foo bar
```
