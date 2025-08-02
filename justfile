
trace trace_name:
    @if [ "{{trace_name}}" = "all" ]; then \
        echo "Running all traces..."; \
        cargo test --lib -- debugger::tracer::tests::test_debugger_traces --exact --show-output --nocapture; \
    else \
        echo "Running trace: {{trace_name}}"; \
        FILTER_TRACE={{trace_name}} cargo test --lib -- debugger::tracer::tests::test_debugger_traces --exact --show-output --nocapture; \
    fi

fuzz-state:
    FUZZ=1 cargo test --lib -- state::fuzz --show-output

# Run the trace test and override the testcases with the generated output
# This is useful for updating the expected traces after changes to the tracer logic
override-traces:
    OVERRIDE_TESTS=1 cargo test --lib -- debugger::tracer::tests::test_debugger_traces --exact --show-output --nocapture

lint:
	cargo clippy -- -D warnings

check-format:
	cargo fmt -- --check

# Generate config documentation
generate-docs:
    ACTION=generate cargo test --package solstice --lib -- config::test::test_generate_config_markdown --exact --show-output

# Validate config documentation is up to date  
check-docs:
    ACTION=validate cargo test --package solstice --lib -- config::test::test_generate_config_markdown --exact --show-output
