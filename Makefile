CARGO ?= cargo

.PHONY: all
all: fmt lint test

.PHONY: bench
bench:
	@RUSTC="$$(rustup which --toolchain nightly rustc)" \
		RUSTFLAGS="--cfg rep_bench" \
		"$$(rustup which --toolchain nightly cargo)" bench

.PHONY: bench-e2e
bench-e2e:
	@./scripts/bench.sh

.PHONY: build
build:
	@$(CARGO) build --release --bin rep

.PHONY: bump
bump:
ifndef VERSION
	$(error VERSION is required, e.g. `make bump VERSION=1.2.3`)
endif
	$(eval VERSION := $(patsubst v%,%,$(VERSION)))
	@if [ "$(VERSION)" = "$$($(CARGO) pkgid | awk -F'[#@]' '{print $$NF}')" ]; then \
		echo "error: Cargo.toml is already at $(VERSION)" >&2; \
		exit 1; \
	fi
	@if [ -d .jj ] && [ "$$(jj show @ -T 'if(empty && description == "", "ok", "dirty")')" != "ok" ]; then \
		jj new; \
	fi
	@sed -i 's/^version = ".*"/version = "$(VERSION)"/' Cargo.toml
	@$(CARGO) update -p rep --offline
	@if [ -d .jj ]; then \
		jj commit -m "Release v$(VERSION)" && \
		git tag -s -m '' "v$(VERSION)" "$$(jj log -r @- --no-graph -T commit_id)" && \
		jj bookmark move main --to @- && \
		jj git push && \
		git push origin "v$(VERSION)"; \
	else \
		git commit -am "Release v$(VERSION)" && \
		git tag -s -m '' "v$(VERSION)" && \
		git push && \
		git push origin "v$(VERSION)"; \
	fi

.PHONY: fmt
fmt:
	@clover format
	@rumdl fmt --quiet
	@$(CARGO) fmt --all

.PHONY: help
help:
	@vhs assets/help.tape

.PHONY: install
install:
	@$(CARGO) install --path .

.PHONY: lint
lint:
	@$(CARGO) clippy -- -D warnings

.PHONY: test
test:
	@$(CARGO) test

.PHONY: update
update:
	@clover run
	@$(CARGO) update
