# CI splits the same check across hosted jobs; local `make check` runs both parts.
COMPONENT ?= all

.PHONY: check check-all check-daemon check-obsidian
check: check-$(COMPONENT)

check-all: check-daemon check-obsidian

check-daemon:
	cd daemon && cargo fmt --all --check
	cd daemon && cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
	cd daemon && cargo test --locked --workspace

check-obsidian:
	npm run check --prefix clients/obsidian
