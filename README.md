# rep

`rep` is a fast find-and-replace tool, based on [fastmod](https://github.com/facebookincubator/fastmod).

Features plain and regex replacement, smart preserve-case rewrites, interactive preview, line deletion, file listing, dry runs, stdin mode, and multiple `-e/--expression` replacements in one pass.

## Install

```shell
brew install gechr/tap/rep
```

Or with Cargo:

```shell
cargo install --git https://github.com/gechr/rep
```

## Usage

Basic replacement:

```sh
rep <find> <replace> [path...]
```

Multiple replacements:

```sh
rep -e <find>=<replace> [-e <find>=<replace> ...] [path...]
```

### Examples

```sh
# Literal replacement
rep foo bar

# Interactive preview
rep --preview foo bar src

# Dry-run summary (no writes)
rep --dry-run foo bar .

# Regex with capture groups
rep --regexp '(foo)\.(bar)' '$2.$1' src

# Multiple expressions in one pass
rep -e foo=bar -e baz=qux src

# Preserve-case rewrite across all 7 variants
rep --smart foo_bar hello_world

# Delete every line containing TODO
rep -d TODO

# Just list files that contain a match
rep -l foo src

# Filter by file type / glob
rep -f rs,go foo bar
rep -f '=Dockerfile' foo bar

# Read from stdin
echo 'hello world' | rep hello goodbye
```

Run `rep --help` for the full option list.
