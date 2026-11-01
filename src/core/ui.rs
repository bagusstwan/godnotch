//! Lightweight, pure-logic UI widget model, hit-testing, and button state machine.
//!
//! # Pure Logic Guarantee
//! This module contains zero Win32 calls and zero external dependencies.
//! It handles widget registration, coordinate transformation, reverse-order hit-testing,
//! bounded scrolling, and strict button state transitions (Normal -> Hover -> Pressed -> Clicked).

#![allow(dead_code)] // Fase 1 T1.8

/// Maximum allowable widgets per open notch frame.
pub const MAX_WIDGETS: usize = 64;

/// Animation progress threshold (0.0..=1.0) required before UI widgets become clickable.
/// Prevents accidental clicks while the notch is expanding or collapsing.
pub const INTERACTION_PROGRESS_THRESHOLD: f32 = 0.95;

/// Default scroll step in logical pixels per single mouse wheel notch (standard 120 WHEEL_DELTA).
pub const DEFAULT_SCROLL_STEP: f32 = 28.0;

/// Two-dimensional logical rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// Creates a new rectangle in logical coordinate units.
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns true if the point (px, py) lies within the rectangle (edges inclusive).
    #[must_use]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= (self.x + self.width) && py >= self.y && py <= (self.y + self.height)
    }
}

/// Category of interactive widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetKind {
    Button,
    ListRow,
}

/// Visual presentation state of a button or interactive row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonVisualState {
    #[default]
    Normal,
    Hover,
    Pressed,
    Disabled,
}

/// An interactive widget registered during open-notch rendering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Widget {
    pub id: u32,
    pub rect: Rect,
    pub kind: WidgetKind,
    pub enabled: bool,
}

impl Widget {
    #[must_use]
    pub const fn new(id: u32, rect: Rect, kind: WidgetKind, enabled: bool) -> Self {
        Self {
            id,
            rect,
            kind,
            enabled,
        }
    }
}

/// Action produced by user interaction with UI widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    Clicked(u32),
}

/// Central state machine managing widget registration, hit-testing, and input transitions.
#[derive(Debug, Clone)]
pub struct UiContext {
    widgets: Vec<Widget>,
    hovered_widget: Option<u32>,
    pressed_widget: Option<u32>,
    is_mouse_down: bool,
    scroll_offset: f32,
    max_scroll: f32,
    scroll_step: f32,
}

impl Default for UiContext {
    fn default() -> Self {
        Self::new()
    }
}

impl UiContext {
    /// Creates a new UI context with pre-allocated widget capacity.
    #[must_use]
    pub fn new() -> Self {
        Self {
            widgets: Vec::with_capacity(MAX_WIDGETS),
            hovered_widget: None,
            pressed_widget: None,
            is_mouse_down: false,
            scroll_offset: 0.0,
            max_scroll: 0.0,
            scroll_step: DEFAULT_SCROLL_STEP,
        }
    }

    /// Clears registered widgets for the new frame while preserving interaction state.
    pub fn clear_widgets(&mut self) {
        self.widgets.clear();
    }

    /// Registers a widget for hit-testing in the current frame.
    ///
    /// Silently drops widgets beyond `MAX_WIDGETS` without panicking.
    /// Returns true if the widget was successfully added.
    pub fn add_widget(&mut self, widget: Widget) -> bool {
        if self.widgets.len() < MAX_WIDGETS {
            self.widgets.push(widget);
            true
        } else {
            false
        }
    }

    /// Returns the number of widgets registered in the current frame.
    #[must_use]
    pub fn widget_count(&self) -> usize {
        self.widgets.len()
    }

    /// Returns the current scroll offset.
    #[must_use]
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_offset
    }

    /// Returns the maximum allowable scroll offset.
    #[must_use]
    pub fn max_scroll(&self) -> f32 {
        self.max_scroll
    }

    /// Configures the scrollable content bounds and updates scroll clamping.
    pub fn set_scroll_bounds(&mut self, content_height: f32, visible_height: f32) {
        self.max_scroll = (content_height - visible_height).max(0.0);
        self.scroll_offset = self.scroll_offset.clamp(0.0, self.max_scroll);
    }

    /// Returns the currently hovered widget ID, if any.
    #[must_use]
    pub fn hovered_widget(&self) -> Option<u32> {
        self.hovered_widget
    }

    /// Returns the currently pressed widget ID, if any.
    #[must_use]
    pub fn pressed_widget(&self) -> Option<u32> {
        self.pressed_widget
    }

    /// Converts physical pixel coordinates (e.g. from Windows mouse messages) to logical coordinates.
    #[must_use]
    pub fn physical_to_logical(px: i32, py: i32, scale: f32) -> (f32, f32) {
        let safe_scale = if scale > 0.01 { scale } else { 1.0 };
        (px as f32 / safe_scale, py as f32 / safe_scale)
    }

    /// Returns true if the notch opening animation has progressed enough to permit UI interaction.
    #[must_use]
    pub fn can_interact(progress: f32, is_animating: bool) -> bool {
        // Must either be fully/substantially open, or settled in open state
        progress >= INTERACTION_PROGRESS_THRESHOLD && (!is_animating || progress >= 0.999)
    }

    /// Evaluates which widget (if any) contains point (x, y) in logical coordinates.
    ///
    /// Evaluates in reverse registration order (latest registered on top).
    /// Disabled widgets are ignored.
    #[must_use]
    pub fn hit_test(&self, x: f32, y: f32) -> Option<u32> {
        for widget in self.widgets.iter().rev() {
            if widget.enabled && widget.rect.contains(x, y) {
                return Some(widget.id);
            }
        }
        None
    }

    /// Handles mouse movement over the notch surface.
    ///
    /// Returns `(dirty, active_hover_id)`.
    pub fn on_mouse_move(&mut self, x: f32, y: f32, can_interact: bool) -> (bool, Option<u32>) {
        if !can_interact {
            let was_hovered = self.hovered_widget.take().is_some();
            return (was_hovered, None);
        }

        let hit = self.hit_test(x, y);
        let changed = hit != self.hovered_widget;
        self.hovered_widget = hit;

        (changed, self.hovered_widget)
    }

    /// Handles left mouse button down events.
    ///
    /// Returns `(dirty, capture_needed)`.
    pub fn on_mouse_down(&mut self, x: f32, y: f32, can_interact: bool) -> (bool, bool) {
        if !can_interact {
            return (false, false);
        }

        self.is_mouse_down = true;
        let hit = self.hit_test(x, y);

        if let Some(id) = hit {
            self.pressed_widget = Some(id);
            self.hovered_widget = Some(id);
            (true, true)
        } else {
            self.pressed_widget = None;
            (false, false)
        }
    }

    /// Handles left mouse button up events.
    ///
    /// Emits `UiAction::Clicked(id)` ONLY if released over the same widget on which
    /// mouse-down occurred. Releasing outside cancels the action.
    ///
    /// Returns `(dirty, action, release_capture)`.
    pub fn on_mouse_up(
        &mut self,
        x: f32,
        y: f32,
        can_interact: bool,
    ) -> (bool, Option<UiAction>, bool) {
        let had_press = self.pressed_widget.take();
        self.is_mouse_down = false;

        let hit = if can_interact {
            self.hit_test(x, y)
        } else {
            None
        };
        let hover_changed = hit != self.hovered_widget;
        self.hovered_widget = hit;

        let action = if let Some(pressed_id) = had_press {
            if hit == Some(pressed_id) {
                Some(UiAction::Clicked(pressed_id))
            } else {
                None
            }
        } else {
            None
        };

        let dirty = had_press.is_some() || hover_changed;
        (dirty, action, true)
    }

    /// Handles loss of mouse capture (e.g. system dialog, Alt+Tab, or external release).
    /// Cancels any active pressed state without triggering an action.
    ///
    /// Returns `true` if state was dirty and requires redrawing.
    pub fn on_capture_lost(&mut self) -> bool {
        let dirty = self.pressed_widget.is_some() || self.is_mouse_down;
        self.pressed_widget = None;
        self.is_mouse_down = false;
        dirty
    }

    /// Handles cursor leaving the notch window.
    ///
    /// If mouse is not pressed down, clears hovered widget.
    /// Returns `true` if state changed.
    pub fn on_mouse_leave(&mut self) -> bool {
        self.hovered_widget.take().is_some()
    }

    /// Handles vertical mouse wheel rotation.
    ///
    /// `delta_lines`: number of lines scrolled (typically 3 lines per wheel click).
    /// Positive moves content up (scroll down); negative moves content down (scroll up).
    ///
    /// Returns `true` if scroll offset changed.
    pub fn on_mouse_wheel(&mut self, delta_lines: f32) -> bool {
        if self.max_scroll <= 0.0 {
            return false;
        }

        let new_offset =
            (self.scroll_offset - delta_lines * self.scroll_step).clamp(0.0, self.max_scroll);
        if (new_offset - self.scroll_offset).abs() > 0.001 {
            self.scroll_offset = new_offset;
            true
        } else {
            false
        }
    }

    /// Resolves the current visual state for a given widget ID.
    #[must_use]
    pub fn button_state(&self, id: u32) -> ButtonVisualState {
        let widget = self.widgets.iter().find(|w| w.id == id);
        match widget {
            Some(w) if !w.enabled => ButtonVisualState::Disabled,
            Some(_) => {
                if self.pressed_widget == Some(id) {
                    if self.hovered_widget == Some(id) {
                        ButtonVisualState::Pressed
                    } else {
                        ButtonVisualState::Normal
                    }
                } else if self.hovered_widget == Some(id) {
                    if self.is_mouse_down {
                        // Another widget was pressed, hovering over this one does not show hover state
                        ButtonVisualState::Normal
                    } else {
                        ButtonVisualState::Hover
                    }
                } else {
                    ButtonVisualState::Normal
                }
            }
            None => ButtonVisualState::Normal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hit_test_points_and_overlap() {
        let mut ui = UiContext::new();

        let w1 = Widget::new(
            1,
            Rect::new(10.0, 10.0, 100.0, 40.0),
            WidgetKind::Button,
            true,
        );
        let w2_overlapping = Widget::new(
            2,
            Rect::new(50.0, 20.0, 100.0, 40.0),
            WidgetKind::Button,
            true,
        );
        let w3_disabled = Widget::new(
            3,
            Rect::new(200.0, 10.0, 80.0, 40.0),
            WidgetKind::Button,
            false,
        );

        assert!(ui.add_widget(w1));
        assert!(ui.add_widget(w2_overlapping));
        assert!(ui.add_widget(w3_disabled));

        // Point inside w1 only (e.g. 20, 20)
        assert_eq!(ui.hit_test(20.0, 20.0), Some(1));

        // Point on exact edge of w1
        assert_eq!(ui.hit_test(10.0, 10.0), Some(1));
        assert_eq!(ui.hit_test(110.0, 50.0), Some(2)); // Within w2 overlapping area

        // Point inside overlapping region (e.g. 60, 30) -> topmost (w2) must win
        assert_eq!(ui.hit_test(60.0, 30.0), Some(2));

        // Point outside all widgets
        assert_eq!(ui.hit_test(5.0, 5.0), None);
        assert_eq!(ui.hit_test(500.0, 500.0), None);

        // Negative coordinates
        assert_eq!(ui.hit_test(-10.0, -20.0), None);

        // Disabled widget is ignored
        assert_eq!(ui.hit_test(220.0, 20.0), None);
    }

    #[test]
    fn test_physical_to_logical_coordinate_scaling() {
        // 100% scale (scale = 1.0)
        let (lx, ly) = UiContext::physical_to_logical(150, 60, 1.0);
        assert!((lx - 150.0).abs() < 1e-4);
        assert!((ly - 60.0).abs() < 1e-4);

        // 125% scale (scale = 1.25)
        let (lx, ly) = UiContext::physical_to_logical(250, 100, 1.25);
        assert!((lx - 200.0).abs() < 1e-4);
        assert!((ly - 80.0).abs() < 1e-4);

        // 150% scale (scale = 1.5)
        let (lx, ly) = UiContext::physical_to_logical(300, 150, 1.5);
        assert!((lx - 200.0).abs() < 1e-4);
        assert!((ly - 100.0).abs() < 1e-4);

        // 200% scale (scale = 2.0)
        let (lx, ly) = UiContext::physical_to_logical(400, 200, 2.0);
        assert!((lx - 200.0).abs() < 1e-4);
        assert!((ly - 100.0).abs() < 1e-4);
    }

    #[test]
    fn test_widget_limit_64_no_panic() {
        let mut ui = UiContext::new();
        for i in 0..MAX_WIDGETS {
            let ok = ui.add_widget(Widget::new(
                i as u32,
                Rect::new(0.0, i as f32 * 10.0, 50.0, 8.0),
                WidgetKind::ListRow,
                true,
            ));
            assert!(ok, "Widget {i} should be accepted");
        }
        assert_eq!(ui.widget_count(), 64);

        // 65th widget must be ignored cleanly
        let overflow_ok = ui.add_widget(Widget::new(
            999,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            WidgetKind::Button,
            true,
        ));
        assert!(!overflow_ok, "65th widget must be rejected");
        assert_eq!(ui.widget_count(), 64);
    }

    #[test]
    fn test_button_state_machine_happy_path() {
        let mut ui = UiContext::new();
        ui.add_widget(Widget::new(
            10,
            Rect::new(20.0, 20.0, 80.0, 30.0),
            WidgetKind::Button,
            true,
        ));

        assert_eq!(ui.button_state(10), ButtonVisualState::Normal);

        // 1. Hover
        let (dirty, hovered) = ui.on_mouse_move(30.0, 25.0, true);
        assert!(dirty);
        assert_eq!(hovered, Some(10));
        assert_eq!(ui.button_state(10), ButtonVisualState::Hover);

        // 2. Mouse down inside
        let (dirty, capture) = ui.on_mouse_down(30.0, 25.0, true);
        assert!(dirty);
        assert!(capture);
        assert_eq!(ui.button_state(10), ButtonVisualState::Pressed);

        // 3. Mouse up inside -> Click action emitted
        let (dirty, action, release_cap) = ui.on_mouse_up(30.0, 25.0, true);
        assert!(dirty);
        assert!(release_cap);
        assert_eq!(action, Some(UiAction::Clicked(10)));
        assert_eq!(ui.button_state(10), ButtonVisualState::Hover);
    }

    #[test]
    fn test_button_press_and_release_outside_cancels() {
        let mut ui = UiContext::new();
        ui.add_widget(Widget::new(
            10,
            Rect::new(20.0, 20.0, 80.0, 30.0),
            WidgetKind::Button,
            true,
        ));

        // Mouse down inside
        let (dirty, capture) = ui.on_mouse_down(30.0, 25.0, true);
        assert!(dirty && capture);
        assert_eq!(ui.button_state(10), ButtonVisualState::Pressed);

        // Move outside while holding down
        let (dirty, hovered) = ui.on_mouse_move(200.0, 200.0, true);
        assert!(dirty);
        assert_eq!(hovered, None);
        assert_eq!(ui.button_state(10), ButtonVisualState::Normal);

        // Release outside -> No action
        let (dirty, action, _) = ui.on_mouse_up(200.0, 200.0, true);
        assert!(dirty);
        assert_eq!(action, None, "Releasing outside must NOT emit action");
        assert_eq!(ui.button_state(10), ButtonVisualState::Normal);
    }

    #[test]
    fn test_press_outside_move_inside_release_cancels() {
        let mut ui = UiContext::new();
        ui.add_widget(Widget::new(
            10,
            Rect::new(20.0, 20.0, 80.0, 30.0),
            WidgetKind::Button,
            true,
        ));

        // Mouse down outside (at 300, 300)
        let (dirty, capture) = ui.on_mouse_down(300.0, 300.0, true);
        assert!(!dirty && !capture);

        // Move inside widget 10
        let (dirty, _) = ui.on_mouse_move(30.0, 25.0, true);
        assert!(dirty);

        // Release inside -> No action because press occurred outside
        let (_, action, _) = ui.on_mouse_up(30.0, 25.0, true);
        assert_eq!(action, None);
    }

    #[test]
    fn test_press_inside_move_out_move_back_in_release_triggers() {
        let mut ui = UiContext::new();
        ui.add_widget(Widget::new(
            10,
            Rect::new(20.0, 20.0, 80.0, 30.0),
            WidgetKind::Button,
            true,
        ));

        // 1. Mouse down inside
        ui.on_mouse_down(30.0, 25.0, true);
        assert_eq!(ui.button_state(10), ButtonVisualState::Pressed);

        // 2. Drag outside
        ui.on_mouse_move(300.0, 300.0, true);
        assert_eq!(ui.button_state(10), ButtonVisualState::Normal);

        // 3. Move back inside
        ui.on_mouse_move(30.0, 25.0, true);
        assert_eq!(ui.button_state(10), ButtonVisualState::Pressed);

        // 4. Release inside -> Action triggered
        let (_, action, _) = ui.on_mouse_up(30.0, 25.0, true);
        assert_eq!(action, Some(UiAction::Clicked(10)));
    }

    #[test]
    fn test_disabled_widget_does_not_react() {
        let mut ui = UiContext::new();
        ui.add_widget(Widget::new(
            20,
            Rect::new(10.0, 10.0, 50.0, 30.0),
            WidgetKind::Button,
            false,
        ));

        assert_eq!(ui.button_state(20), ButtonVisualState::Disabled);

        // Hover over disabled
        let (dirty, hovered) = ui.on_mouse_move(20.0, 20.0, true);
        assert!(!dirty);
        assert_eq!(hovered, None);
        assert_eq!(ui.button_state(20), ButtonVisualState::Disabled);

        // Down on disabled
        let (dirty, capture) = ui.on_mouse_down(20.0, 20.0, true);
        assert!(!dirty);
        assert!(!capture);

        // Up on disabled
        let (_, action, _) = ui.on_mouse_up(20.0, 20.0, true);
        assert_eq!(action, None);
        assert_eq!(ui.button_state(20), ButtonVisualState::Disabled);
    }

    #[test]
    fn test_capture_lost_mid_press_cancels_action() {
        let mut ui = UiContext::new();
        ui.add_widget(Widget::new(
            10,
            Rect::new(20.0, 20.0, 80.0, 30.0),
            WidgetKind::Button,
            true,
        ));

        // Press down
        ui.on_mouse_down(30.0, 25.0, true);
        assert_eq!(ui.button_state(10), ButtonVisualState::Pressed);

        // Capture lost (e.g. system focus switch or Alt+Tab)
        let dirty = ui.on_capture_lost();
        assert!(dirty);
        assert_eq!(ui.pressed_widget(), None);
        assert_eq!(ui.button_state(10), ButtonVisualState::Hover);

        // Subsequent mouse up produces no action
        let (_, action, _) = ui.on_mouse_up(30.0, 25.0, true);
        assert_eq!(action, None);
    }

    #[test]
    fn test_rapid_sequential_clicks() {
        let mut ui = UiContext::new();
        ui.add_widget(Widget::new(
            10,
            Rect::new(10.0, 10.0, 60.0, 30.0),
            WidgetKind::Button,
            true,
        ));

        let mut click_count = 0;
        for _ in 0..10 {
            ui.on_mouse_down(20.0, 20.0, true);
            let (_, action, _) = ui.on_mouse_up(20.0, 20.0, true);
            if let Some(UiAction::Clicked(10)) = action {
                click_count += 1;
            }
        }
        assert_eq!(
            click_count, 10,
            "10 sequential down/up pairs must yield 10 clicks"
        );
    }

    #[test]
    fn test_scroll_bounds_and_clamping() {
        let mut ui = UiContext::new();

        // Content shorter than visible height: max_scroll = 0, no scroll
        ui.set_scroll_bounds(100.0, 150.0);
        assert_eq!(ui.max_scroll(), 0.0);
        assert_eq!(ui.scroll_offset(), 0.0);
        assert!(!ui.on_mouse_wheel(1.0));
        assert!(!ui.on_mouse_wheel(-1.0));

        // Content longer: 300px content, 100px visible -> max_scroll = 200px
        ui.set_scroll_bounds(300.0, 100.0);
        assert_eq!(ui.max_scroll(), 200.0);
        assert_eq!(ui.scroll_offset(), 0.0);

        // Scroll down (positive delta -> moves down in content, increasing offset)
        // delta_lines = -1.0 in Windows mouse wheel terminology is scrolling down
        // On wheel down (-120 WHEEL_DELTA -> -1 line), offset increases
        let dirty = ui.on_mouse_wheel(-1.0);
        assert!(dirty);
        assert_eq!(ui.scroll_offset(), DEFAULT_SCROLL_STEP);

        // Scroll several times
        for _ in 0..10 {
            ui.on_mouse_wheel(-1.0);
        }
        // Offset clamped to max_scroll (200.0)
        assert_eq!(ui.scroll_offset(), 200.0);

        // Attempting to scroll past bottom returns false (no dirty redraw)
        assert!(!ui.on_mouse_wheel(-1.0));

        // Scroll back up
        let dirty = ui.on_mouse_wheel(2.0);
        assert!(dirty);
        assert_eq!(ui.scroll_offset(), 200.0 - 2.0 * DEFAULT_SCROLL_STEP);

        // Scroll past top clamped to 0.0
        for _ in 0..10 {
            ui.on_mouse_wheel(1.0);
        }
        assert_eq!(ui.scroll_offset(), 0.0);
        assert!(!ui.on_mouse_wheel(1.0));
    }

    #[test]
    fn test_interaction_during_opening_transition_rejected() {
        let mut ui = UiContext::new();
        ui.add_widget(Widget::new(
            1,
            Rect::new(10.0, 10.0, 50.0, 30.0),
            WidgetKind::Button,
            true,
        ));

        // Mid-transition: progress = 0.5 (animating)
        assert!(!UiContext::can_interact(0.5, true));
        let (_, hit) = ui.on_mouse_move(20.0, 20.0, false);
        assert_eq!(hit, None);

        let (dirty, cap) = ui.on_mouse_down(20.0, 20.0, false);
        assert!(!dirty && !cap);

        let (_, action, _) = ui.on_mouse_up(20.0, 20.0, false);
        assert_eq!(action, None);

        // Settled: progress = 1.0 (not animating)
        assert!(UiContext::can_interact(1.0, false));
        let (_, hit) = ui.on_mouse_move(20.0, 20.0, true);
        assert_eq!(hit, Some(1));
    }
}
