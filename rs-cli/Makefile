# LambdaDoom `ldoom` CLI — build helpers.
#
# Most people never need this: `./deploy.sh` downloads a prebuilt binary. Build from
# source only if you want to. The proxy/open path needs the `proxy` feature.
#
# Run the built binary from the REPO ROOT (one level up) so `ldoom build` finds ./capsule:
#   make release && (cd .. && rs-cli/target/release/ldoom build --name doom)

FEATURES ?= proxy

.PHONY: build release test fmt clippy clean

build:        ## Debug build (with the proxy feature)
	cargo build --features $(FEATURES)

release:      ## Optimized build -> target/release/ldoom
	cargo build --release --features $(FEATURES)

test:         ## Run the unit tests
	cargo test --features $(FEATURES)

fmt:          ## Format
	cargo fmt

clippy:       ## Lint
	cargo clippy --features $(FEATURES) -- -D warnings

clean:        ## Remove build artifacts
	cargo clean
