//! Small reusable UI pieces: dropdowns, buttons, labels.

use std::rc::Rc;

use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, MouseButton, Pixels, SharedString, Size, Stateful, Window,
    anchored, deferred, div, point, prelude::*, px,
};

use crate::theme::{self, color, icon};

#[derive(Clone)]
pub struct DropdownItem {
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub selected: bool,
}

impl DropdownItem {
    pub fn new(label: impl Into<SharedString>, selected: bool) -> Self {
        Self { label: label.into(), detail: None, selected }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A select-style dropdown. Its open/closed state lives in the parent view
/// (only one menu is open at a time), which is passed in as `open`.
pub struct Dropdown {
    id: &'static str,
    value: SharedString,
    placeholder: bool,
    items: Vec<DropdownItem>,
    menu_width: Pixels,
    disabled: bool,
    empty_text: SharedString,
}

impl Dropdown {
    pub fn new(id: &'static str, value: impl Into<SharedString>, items: Vec<DropdownItem>) -> Self {
        Self {
            id,
            value: value.into(),
            placeholder: false,
            items,
            menu_width: px(220.),
            disabled: false,
            empty_text: "Nothing to choose".into(),
        }
    }

    pub fn menu_width(mut self, width: Pixels) -> Self {
        self.menu_width = width;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Shows `value` dimmed, as a prompt rather than a choice.
    pub fn placeholder(mut self, placeholder: bool) -> Self {
        self.placeholder = placeholder;
        self
    }

    pub fn empty_text(mut self, text: impl Into<SharedString>) -> Self {
        self.empty_text = text.into();
        self
    }

    pub fn render<T: 'static>(
        self,
        open: bool,
        viewport: Size<Pixels>,
        set_open: impl Fn(&mut T, Option<&'static str>, &mut Context<T>) + 'static,
        on_select: impl Fn(&mut T, usize, &mut Window, &mut Context<T>) + 'static,
        cx: &mut Context<T>,
    ) -> AnyElement {
        let id = self.id;
        let set_open = Rc::new(set_open);
        let on_select = Rc::new(on_select);

        let trigger = div()
            .id(ElementId::Name(format!("{id}-trigger").into()))
            .h(px(32.))
            .w_full()
            .px_2p5()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .bg(color(theme::ELEVATED))
            .border_1()
            .border_color(if open { theme::accent() } else { color(theme::BORDER_STRONG) })
            .text_sm()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(color(if self.placeholder { theme::MUTED } else { theme::TEXT }))
                    .child(self.value),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme::ICONS)
                    .text_size(px(10.))
                    .text_color(color(theme::MUTED))
                    .child(icon::CHEVRON_DOWN),
            )
            .map(|this| {
                if self.disabled {
                    this.opacity(0.45)
                } else {
                    let set_open = set_open.clone();
                    this.cursor_pointer().hover(|s| s.bg(color(theme::HOVER))).on_click(
                        cx.listener(move |this, _, _, cx| set_open(this, if open { None } else { Some(id) }, cx)),
                    )
                }
            });

        let menu = open.then(|| {
            let items = self.items.into_iter().enumerate().map(|(i, item)| {
                let on_select = on_select.clone();
                let set_open = set_open.clone();
                div()
                    .id(ElementId::Name(format!("{id}-item-{i}").into()))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_sm()
                    .when(item.selected, |this| this.bg(theme::accent_soft()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_sm().truncate().text_color(color(theme::TEXT)).child(item.label))
                            .when_some(item.detail, |this, detail| {
                                this.child(div().text_xs().truncate().text_color(color(theme::MUTED)).child(detail))
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.))
                            .font_family(theme::ICONS)
                            .text_size(px(11.))
                            .text_color(theme::accent())
                            .when(item.selected, |this| this.child(icon::CHECK)),
                    )
                    .cursor_pointer()
                    .hover(|s| s.bg(color(theme::HOVER)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        set_open(this, None, cx);
                        on_select(this, i, window, cx);
                    }))
            });
            let items: Vec<_> = items.collect();
            let empty = items.is_empty();

            div()
                .id(ElementId::Name(format!("{id}-menu").into()))
                .occlude()
                .mt_1()
                .w(self.menu_width)
                .max_h(px(340.))
                .overflow_y_scroll()
                .p_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .rounded_lg()
                .bg(color(theme::ELEVATED))
                .border_1()
                .border_color(color(theme::BORDER_STRONG))
                .shadow_lg()
                .when(empty, |this| {
                    this.child(div().px_2().py_1p5().text_sm().text_color(color(theme::MUTED)).child(self.empty_text))
                })
                .children(items)
        });

        let close = set_open.clone();
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .child(trigger)
            .when_some(menu, |this, menu| {
                this
                    // Invisible full-window layer: clicking anywhere else closes the menu.
                    .child(
                        deferred(
                            anchored().position(point(px(0.), px(0.))).child(
                                div()
                                    .id(ElementId::Name(format!("{id}-backdrop").into()))
                                    .w(viewport.width)
                                    .h(viewport.height)
                                    .occlude()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| close(this, None, cx)),
                                    ),
                            ),
                        )
                        .with_priority(1),
                    )
                    .child(
                        div().absolute().top_full().left_0().child(
                            deferred(anchored().snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2),
                        ),
                    )
            })
            .into_any_element()
    }
}

pub fn section_title(label: &'static str) -> Div {
    div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(color(theme::SUBTLE)).child(label.to_uppercase())
}

/// A "label ......... [control]" settings row.
pub fn setting_row(label: &'static str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(div().text_sm().text_color(color(theme::MUTED)).child(label))
        .child(control)
}

/// Small explanatory text under a setting.
pub fn note(text: impl Into<SharedString>, warning: bool) -> Div {
    div()
        .w_full()
        .flex()
        .gap_1p5()
        .text_xs()
        .text_color(color(if warning { theme::WARNING } else { theme::MUTED }))
        .child(div().flex_none().pt(px(2.)).font_family(theme::ICONS).text_size(px(10.)).child(icon::WARNING))
        .child(div().flex_1().min_w_0().child(text.into()))
}

/// A square button showing an icon glyph.
pub fn icon_button(id: &'static str, glyph: &'static str, disabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(32.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(color(theme::ELEVATED))
        .border_1()
        .border_color(color(theme::BORDER_STRONG))
        .font_family(theme::ICONS)
        .text_size(px(12.))
        .text_color(color(theme::TEXT))
        .child(glyph)
        .map(
            |this| {
                if disabled { this.opacity(0.45) } else { this.cursor_pointer().hover(|s| s.bg(color(theme::HOVER))) }
            },
        )
}

/// A small text tooltip (use with `.tooltip(Tooltip::text(...))`).
pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    pub fn text(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView {
        let text = text.into();
        move |_, cx| cx.new(|_| Tooltip { text: text.clone() }).into()
    }
}

impl gpui::Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(280.))
            .px_2p5()
            .py_1p5()
            .rounded_md()
            .bg(color(theme::ELEVATED))
            .border_1()
            .border_color(color(theme::BORDER_STRONG))
            .shadow_lg()
            .text_xs()
            .text_color(color(theme::TEXT))
            .child(self.text.clone())
    }
}

/// A low-emphasis button with an optional icon.
pub fn ghost_button(
    id: &'static str,
    glyph: Option<&'static str>,
    label: &'static str,
    disabled: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .h(px(32.))
        .px_3()
        .rounded_md()
        .text_sm()
        .text_color(color(theme::TEXT))
        .bg(color(theme::ELEVATED))
        .border_1()
        .border_color(color(theme::BORDER_STRONG))
        .when_some(glyph, |this, glyph| {
            this.child(div().font_family(theme::ICONS).text_size(px(12.)).text_color(color(theme::MUTED)).child(glyph))
        })
        .child(label)
        .map(
            |this| {
                if disabled { this.opacity(0.45) } else { this.cursor_pointer().hover(|s| s.bg(color(theme::HOVER))) }
            },
        )
}
