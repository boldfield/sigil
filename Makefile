.PHONY: check test

check:
	@bash scripts/pod-verify.sh

test:
	@cargo test -p sigil-compiler --test e2e
