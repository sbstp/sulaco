.PHONY: debug
debug:
	cargo build --target x86_64-unknown-linux-musl

.PHONY: release
release:
	cargo build --release --target x86_64-unknown-linux-musl
	strip target/x86_64-unknown-linux-musl/release/sulaco

.PHONY: docker-tests
docker-tests: debug
	docker build -f tests/zombie/Dockerfile -t sulaco_zombier_maker .
	docker run --rm sulaco_zombier_maker
