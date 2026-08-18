use kithara_ui::render::{Node, ReadValue, Scope};

use super::value::Value;
use crate::gui::ui::{
    cache::{CollapsedModules, DeckLayout},
    menu::MenuState,
    modules::Modules,
    scope::deck_index,
    window::WindowState,
};

#[derive(Clone, Copy)]
pub(super) struct UiNode<'a> {
    collapsed: &'a CollapsedModules,
    layout: DeckLayout,
    menu: &'a MenuState,
    modules: &'a Modules,
    window: &'a WindowState,
    drag: DragNode<'a>,
}

impl<'a> UiNode<'a> {
    pub(super) const fn new(
        drag: DragNode<'a>,
        layout: DeckLayout,
        collapsed: &'a CollapsedModules,
        menu: &'a MenuState,
        modules: &'a Modules,
        window: &'a WindowState,
    ) -> Self {
        Self {
            collapsed,
            layout,
            menu,
            modules,
            window,
            drag,
        }
    }
}

impl<'a> Node<'a> for UiNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let node: Box<dyn Node<'a> + 'a> = match segment {
            "app" => Box::new(AppNode),
            "drag" => Box::new(self.drag),
            "layout" => Box::new(LayoutNode {
                layout: self.layout,
            }),
            "layouts" => Box::new(LayoutsNode {
                layout: self.layout,
            }),
            "menu" => Box::new(MenuNode { menu: self.menu }),
            "window" => Box::new(WindowNode {
                window: self.window,
            }),
            "module" => Box::new(ModulesNode {
                collapsed: self.collapsed,
                modules: self.modules,
            }),
            "modules" => Box::new(ModuleCountNode {
                modules: self.modules,
            }),
            _ => return None,
        };
        Some(node)
    }
}

#[derive(Clone, Copy)]
struct AppNode;

impl<'a> Node<'a> for AppNode {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "version" => ReadValue::Text(env!("CARGO_PKG_VERSION")),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

/// The app runs one window, so the menu's window list is that window: its
/// layout, its size, and the modules it lays out. It is always the active one
/// and never closable.
#[derive(Clone, Copy)]
struct WindowNode<'a> {
    window: &'a WindowState,
}

impl<'a> Node<'a> for WindowNode<'a> {
    fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let only = scope.get("window") == Some("1");
        let value = match segment {
            "count" => ReadValue::Text("1 WINDOW"),
            "active" => ReadValue::Bool(only),
            "close_hidden" => ReadValue::Bool(true),
            "title" => ReadValue::Text(self.window.title()),
            "caption" => ReadValue::Text(self.window.caption()),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct MenuNode<'a> {
    menu: &'a MenuState,
}

impl<'a> Node<'a> for MenuNode<'a> {
    fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "open" => ReadValue::Bool(self.menu.is_open()),
            "group_open" => ReadValue::Bool(self.group_open(scope)),
            "group_hidden" => ReadValue::Bool(!self.group_open(scope)),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

impl MenuNode<'_> {
    /// The menu expands one group at a time; a group it does not draw is
    /// closed.
    fn group_open(self, scope: Scope<'_>) -> bool {
        match scope.get("group") {
            Some("lay") => self.menu.are_layouts_open(),
            Some("mod") => self.menu.are_modules_open(),
            _ => false,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct DragNode<'a> {
    over: Option<usize>,
    track: Option<&'a str>,
    decks: usize,
}

impl<'a> DragNode<'a> {
    pub(super) const fn new(track: Option<&'a str>, over: Option<usize>, decks: usize) -> Self {
        Self { over, track, decks }
    }
}

impl<'a> Node<'a> for DragNode<'a> {
    fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "track" => ReadValue::Text(self.track?),
            "over" => {
                let deck = deck_index(scope.get("deck")?).filter(|deck| *deck < self.decks)?;
                ReadValue::Bool(self.over == Some(deck))
            }
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct LayoutNode {
    layout: DeckLayout,
}

impl<'a> Node<'a> for LayoutNode {
    fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "selected" => ReadValue::Bool(self.is_selected(scope)),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

impl LayoutNode {
    /// A menu row names its layout by deck count; a count the app has no
    /// layout for is never the one in force.
    fn is_selected(self, scope: Scope<'_>) -> bool {
        scope
            .get("layout")
            .and_then(|decks| decks.parse().ok())
            .and_then(DeckLayout::from_decks)
            == Some(self.layout)
    }
}

#[derive(Clone, Copy)]
struct LayoutsNode {
    layout: DeckLayout,
}

impl<'a> Node<'a> for LayoutsNode {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "active" => ReadValue::Text(self.layout.label()),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct ModulesNode<'a> {
    collapsed: &'a CollapsedModules,
    modules: &'a Modules,
}

impl<'a> Node<'a> for ModulesNode<'a> {
    /// `on` and `hidden` are the scoped reads the menu grid binds; any other
    /// segment is a module document id whose chrome reports its own collapse.
    fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "on" => ReadValue::Bool(self.is_on(scope)),
            "hidden" => ReadValue::Bool(!self.is_on(scope)),
            document => {
                return Some(Box::new(ModuleNode {
                    collapsed: self.collapsed.contains(document),
                }));
            }
        };
        Some(Box::new(Value(value)))
    }
}

impl ModulesNode<'_> {
    fn is_on(&self, scope: Scope<'_>) -> bool {
        scope
            .get("module")
            .is_some_and(|module| self.modules.is_on(module))
    }
}

#[derive(Clone, Copy)]
struct ModuleCountNode<'a> {
    modules: &'a Modules,
}

impl<'a> Node<'a> for ModuleCountNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "count" => ReadValue::Text(self.modules.count()),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct ModuleNode {
    collapsed: bool,
}

impl<'a> Node<'a> for ModuleNode {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "collapsed" => ReadValue::Bool(self.collapsed),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}
