CARGO ?= cargo

.PHONY: all
all: fmt lint test

.PHONY: fmt
fmt:
	@rumdl fmt --quiet
	@$(CARGO) fmt --all

.PHONY: build
build:
	@$(CARGO) build --release --bin rep

.PHONY: install
install:
	@$(CARGO) install --path .
	@rep --completions fish > ~/.config/fish/completions/rep.fish

.PHONY: lint
lint:
	@$(CARGO) clippy -- -D warnings

.PHONY: test
test:
	@$(CARGO) test

.PHONY: update
update:
	@$(CARGO) update
