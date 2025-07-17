
# Example recipe that takes a required input
trace trace_name:
    FILTER_TRACE={{trace_name}} cargo test --package solstice --bin solstice -- tracer::tests::test_debugger_traces --exact --show-output --nocapture

fuzz-state:
    FUZZ=1 cargo test --package solstice --bin solstice -- state::fuzz --show-output
