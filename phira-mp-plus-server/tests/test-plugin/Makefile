# Build the test WASM plugin and produce a component binary.
# Usage:
#   make          — build + wrap
#   make clean    — remove build artifacts
#   make rebuild  — clean + build
#
# Prerequisites:
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-tools

TARGET_DIR := target
WASM_BIN := $(TARGET_DIR)/wasm32-unknown-unknown/release/test_plugin.wasm
COMPONENT_BIN := ../test-plugin.component.wasm

.PHONY: all clean rebuild

all: $(COMPONENT_BIN)

$(WASM_BIN): src/lib.rs Cargo.toml
	cargo build --target wasm32-unknown-unknown --release

$(COMPONENT_BIN): $(WASM_BIN)
	wasm-tools component new $(WASM_BIN) -o $(COMPONENT_BIN)
	@echo "=== Component binary size: $$(wc -c < $(COMPONENT_BIN)) bytes ==="

clean:
	cargo clean
	rm -f $(COMPONENT_BIN)

rebuild: clean all
