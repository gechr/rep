# rep

`rep` is a fast find-and-replace tool, based on [fastmod](https://github.com/facebookincubator/fastmod).

Features plain and regex replacement, case-aware rewrites (`--smart` for identifiers, `--preserve` for mirroring source case), interactive preview, line deletion, file listing, dry runs, stdin mode, and multiple `-e/--expression` replacements in one pass.

## Install

```shell
brew install gechr/tap/rep
```

Or with Cargo:

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

## Case-aware replacement

`rep` offers two strategies for case-aware replacement. They solve different problems and are mutually exclusive - passing both makes the later flag win (so a flag in `~/.reprc` can be overridden on the command line).

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

`rep` reads default flags from `~/.reprc` if it exists. The file is plain - one CLI flag per line, blank lines and `#` comments ignored. Flags from the rc are prepended to argv before parsing, so command-line arguments override rc values.

```sh
# ~/.reprc
--hidden
--ignore-case
--preview-tool=delta
--hyperlink-format=vscode
```

Set `REP_CONFIG_PATH` to use a different file (e.g. for project-local config), or set it to an empty string to disable rc loading entirely.

```sh
REP_CONFIG_PATH=./.reprc rep foo bar
```
