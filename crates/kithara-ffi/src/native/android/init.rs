use jni::{Env, objects::JObject, sys::jint};
use tracing_subscriber::{filter::LevelFilter, prelude::*};

pub(super) fn run(
    env: &mut Env<'_>,
    context: JObject<'_>,
    log_level: jint,
) -> jni::errors::Result<()> {
    install_logging(log_level);

    let vm = env.get_java_vm()?;
    let context_ref = env.new_global_ref(context)?;
    kithara_android::initialize(&vm, context_ref);
    Ok(())
}

fn install_logging(log_level: jint) {
    let Ok(layer) = tracing_android::layer("kithara") else {
        return;
    };
    let _ = tracing_subscriber::registry()
        .with(layer.with_filter(level_filter(log_level)))
        .try_init();
}

fn level_filter(ordinal: jint) -> LevelFilter {
    const LOG_LEVEL_INFO: jint = 2;
    const LOG_LEVEL_WARN: jint = 3;
    const LOG_LEVEL_ERROR: jint = 4;

    match ordinal {
        0 => LevelFilter::TRACE,
        1 => LevelFilter::DEBUG,
        LOG_LEVEL_INFO => LevelFilter::INFO,
        LOG_LEVEL_WARN => LevelFilter::WARN,
        LOG_LEVEL_ERROR => LevelFilter::ERROR,
        _ => LevelFilter::OFF,
    }
}
