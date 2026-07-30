use std::{env, fs, path::PathBuf};

use flow::runtime::{ResourceId, start};

#[test]
fn writes_architecture_trace() {
    start(ResourceId(7));

    let path = PathBuf::from(env::var_os("ARCHITECTURE_TRACE_PATH").expect("trace path"));
    let trace = concat!(
        "{\"schema_version\":1,\"sequence\":1,\"kind\":\"span_enter\",\"name\":\"start\",",
        "\"source\":{\"path\":\"flow/src/runtime.rs\",\"line\":25,\"column\":7},",
        "\"span_id\":\"start\"}\n",
        "{\"schema_version\":1,\"sequence\":2,\"kind\":\"resource_create\",\"name\":\"start\",",
        "\"source\":{\"path\":\"flow/src/runtime.rs\",\"line\":25,\"column\":7},",
        "\"parent_span_id\":\"start\",\"resource_id\":\"state-7\",",
        "\"resource_type\":\"SharedState\"}\n",
        "{\"schema_version\":1,\"sequence\":3,\"kind\":\"task_spawn\",\"name\":\"start\",",
        "\"source\":{\"path\":\"flow/src/runtime.rs\",\"line\":25,\"column\":7},",
        "\"parent_span_id\":\"start\",\"task_id\":\"worker-task\"}\n",
        "{\"schema_version\":1,\"sequence\":4,\"kind\":\"span_enter\",\"name\":\"worker\",",
        "\"source\":{\"path\":\"flow/src/runtime.rs\",\"line\":34,\"column\":3},",
        "\"span_id\":\"worker\",\"parent_span_id\":\"start\",\"task_id\":\"worker-task\"}\n",
        "{\"schema_version\":1,\"sequence\":5,\"kind\":\"send\",\"name\":\"worker\",",
        "\"source\":{\"path\":\"flow/src/runtime.rs\",\"line\":34,\"column\":3},",
        "\"parent_span_id\":\"worker\",\"task_id\":\"worker-task\",",
        "\"correlation_id\":\"resource-ready\",\"resource_id\":\"resource-7\",",
        "\"resource_type\":\"ResourceId\"}\n",
        "{\"schema_version\":1,\"sequence\":6,\"kind\":\"receive\",\"name\":\"start\",",
        "\"source\":{\"path\":\"flow/src/runtime.rs\",\"line\":25,\"column\":7},",
        "\"parent_span_id\":\"start\",\"correlation_id\":\"resource-ready\",",
        "\"resource_id\":\"resource-7\",\"resource_type\":\"ResourceId\"}\n"
    );
    fs::write(path, trace).expect("write trace");
}
