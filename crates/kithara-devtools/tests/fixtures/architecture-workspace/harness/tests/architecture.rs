use std::{env, fs, path::PathBuf};

use flow::runtime::{ResourceId, start};

#[test]
fn writes_architecture_trace() {
    start(ResourceId(9));

    let path = PathBuf::from(env::var_os("ARCHITECTURE_TRACE_PATH").expect("trace path"));
    let trace = concat!(
        "{\"schema_version\":1,\"sequence\":1,\"kind\":\"span_enter\",\"name\":\"start\",",
        "\"source\":{\"path\":\"flow/src/runtime.rs\",\"line\":25,\"column\":7},",
        "\"span_id\":\"start\"}\n",
        "{\"schema_version\":1,\"sequence\":2,\"kind\":\"task_spawn\",\"name\":\"start\",",
        "\"source\":{\"path\":\"flow/src/runtime.rs\",\"line\":25,\"column\":7},",
        "\"parent_span_id\":\"start\",\"task_id\":\"worker-task\"}\n",
        "{\"schema_version\":1,\"sequence\":3,\"kind\":\"span_enter\",\"name\":\"worker\",",
        "\"source\":{\"path\":\"flow/src/runtime.rs\",\"line\":34,\"column\":3},",
        "\"span_id\":\"worker\",\"parent_span_id\":\"start\",\"task_id\":\"worker-task\"}\n"
    );
    fs::write(path, trace).expect("write trace");
}
