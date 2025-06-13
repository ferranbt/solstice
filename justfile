
# Example recipe that takes a required input
trace trace_name:
    FILTER_TRACE={{trace_name}} cargo test --package solstice --bin solstice -- tracer::tests::test_debugger_traces --exact --show-output --nocapture
