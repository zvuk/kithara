use super::*;

pub(crate) fn spawn<S, O>(
    root: HostRoot,
    view: RootView,
    output_block_frames: Option<NonZeroU32>,
    channel_config: kithara_command::ScopedConfig,
    output: SessionOutput,
    settings: Live<HostSettings, HostProtocol>,
    layer: impl FnOnce(HostCore<S, O::Deck>) -> O + MaybeSend + 'static,
) -> Arc<SessionClient<O::Command>>
where
    S: HasPool<f32> + Send + Sync + 'static,
    O: HostOwner<S>,
{
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (postbox, mut mailbox) = mailbox();
    mailbox.hold(Waker::from(Arc::new(SessionWake(Mutex::new(
        cmd_tx.clone(),
    )))));
    let client = Arc::new(SessionClient {
        postbox,
        cmd_tx: Mutex::new(cmd_tx),
    });
    let inbox: Arc<dyn DeckInbox> = client.clone();
    spawn_named("host-deck-session", move || {
        let start = move |ctx: &mut FirewheelContext, rate| {
            start_stream_cpal(ctx, rate, output_block_frames).map(|backend| {
                SessionStream::Realtime {
                    _backend: Box::new(backend),
                }
            })
        };
        let mut state = SessionState::new(
            root,
            view,
            SessionBufferConfig {
                max_block_frames: output_block_frames,
                declick_frames: None,
            },
            output,
            settings,
            channel_config,
            start,
        );
        state.delivery_delay = Some(consts::SESSION_PUMP_INTERVAL);
        engine_thread(&cmd_rx, mailbox, layer(HostCore::new(state, inbox)));
    });
    client
}

fn start_stream_cpal(
    ctx: &mut FirewheelContext,
    sample_rate: u32,
    output_block_frames: Option<NonZeroU32>,
) -> Result<CpalStream, String> {
    debug!(sample_rate, "starting cpal stream");
    CpalStream::new(ctx, cpal_config(sample_rate, output_block_frames))
        .map_err(|error| error.to_string())
}
