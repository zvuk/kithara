//! What every recording host in this suite mounts the same way: containers
//! concatenate what their children recorded and wrappers pass their child on.

/// What `groups` recorded, in mount order.
pub(crate) fn flatten<T>(groups: impl IntoIterator<Item = Vec<T>>) -> Vec<T> {
    groups.into_iter().flatten().collect()
}

/// The `Host` methods of a recording host whose output is a `Vec`: `slot`,
/// `split` and `stage` concatenate their children, `pressable`, `scroll` and
/// `window` pass their child on.
macro_rules! pass_through_mounts {
    () => {
        fn pressable(
            &mut self,
            _path: ::kithara_ui::ids::InternId,
            _presses: ::kithara_ui::render::document::Presses,
            _owner: ::kithara_ui::render::InputOwner,
            child: Self::Output,
            _size: Option<::kithara_ui::size::SizeSpec>,
        ) -> Self::Output {
            child
        }

        fn scroll(
            &mut self,
            _id: ::kithara_ui::ids::InternId,
            child: Self::Output,
            _size: Option<::kithara_ui::size::SizeSpec>,
        ) -> Self::Output {
            child
        }

        fn slot(
            &mut self,
            children: Vec<::kithara_ui::render::document::GroupMount<Self::Output>>,
            _size: Option<::kithara_ui::size::SizeSpec>,
        ) -> Self::Output {
            $crate::mount::flatten(children.into_iter().map(|cell| cell.output))
        }

        fn split(
            &mut self,
            _axis: ::kithara_ui::layout::Axis,
            _measure: Option<::kithara_ui::module::MeasureAxis>,
            children: Vec<::kithara_ui::render::document::SplitMount<Self::Output>>,
        ) -> Self::Output {
            $crate::mount::flatten(children.into_iter().map(|cell| cell.output))
        }

        fn stage(
            &mut self,
            children: Vec<::kithara_ui::render::document::StageMount<Self::Output>>,
            _size: Option<::kithara_ui::size::SizeSpec>,
        ) -> Self::Output {
            $crate::mount::flatten(children.into_iter().map(|child| child.output))
        }

        fn window(&mut self, content: Self::Output, _resize_edges: bool) -> Self::Output {
            content
        }
    };
}

pub(crate) use pass_through_mounts;
