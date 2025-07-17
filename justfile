
trace trace_name:
    FILTER_TRACE={{trace_name}} cargo test --lib -- tracer::tests::test_debugger_traces --exact --show-output --nocapture

fuzz-state:
    FUZZ=1 cargo test --lib -- state::fuzz --show-output

# Run the trace test and override the testcases with the generated output
# This is useful for updating the expected traces after changes to the tracer logic
override-traces:
    OVERRIDE_TESTS=1 cargo test --lib -- tracer::tests::test_debugger_traces --exact --show-output --nocapture
