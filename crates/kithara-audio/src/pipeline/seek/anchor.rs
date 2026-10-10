use kithara_stream::ContainerFormat;

pub(crate) fn variant_boundary(
    changed: bool,
    same_codec: bool,
    container: Option<ContainerFormat>,
) -> bool {
    changed
        && (!same_codec
            || container.is_some_and(|format| format != ContainerFormat::Wav && needs_init(format)))
}

pub(crate) const fn needs_init(container: ContainerFormat) -> bool {
    matches!(
        container,
        ContainerFormat::Fmp4
            | ContainerFormat::Mp4
            | ContainerFormat::Wav
            | ContainerFormat::Mkv
            | ContainerFormat::Caf
    )
}
