# rep

`rep` is a fast find-and-replace tool, based on [fastmod](https://github.com/facebookincubator/fastmod).

It supports plain string replacement, regex replacement, interactive preview, dry-run summaries, and repeatable `-e/--expression` replacements.

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

Examples:

```sh
rep foo bar
rep --preview foo bar src
rep --dry-run foo bar .
rep --regexp '(foo)\.(bar)' '$2.$1' src
rep -e foo=bar -e baz=qux src
```

## Development

```sh
make
```
