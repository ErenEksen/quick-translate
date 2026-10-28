//! First-pointer-wins placement across the per-output overlays (design D3, spike 2.1).
//!
//! The first overlay that learns the pointer position wins and gets the panel; the others are
//! removed. If no position arrives within [`FALLBACK_DELAY`] after the first overlay maps, the
//! panel is centered on the overlay the compositor placed itself (requested without an output,
//! so it lands on the compositor's active output). Some compositors (Sway) only send
//! `enter` on motion, so a late `enter` within [`LATE_ENTER`] of the fallback, before any user
//! input, still moves the panel to the pointer; the other overlays stay alive (without
//! keyboard) until then.

use std::time::{Duration, Instant};

use iced::{Point, Rectangle, Size};

use super::geometry;
use super::overlay::PointerKind;

/// Wait after the first overlay is mapped before centering the panel.
pub const FALLBACK_DELAY: Duration = Duration::from_millis(50);
/// How long after a centered fallback a late pointer `enter` may still move the panel.
pub const LATE_ENTER: Duration = Duration::from_millis(300);

/// Outcome of the [`FALLBACK_DELAY`] timer.
#[derive(Debug, Clone, PartialEq)]
pub enum Fallback<Id> {
    /// The pointer already won, or the fallback overlay is still mapping (it is placed when it
    /// opens).
    Nothing,
    /// Request an overlay without an output and register it with [`Placement::requested`];
    /// the panel is centered on it once it opens.
    NeedSurface,
    Placed(Placed<Id>),
}

/// What the shell must do after a placement decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed<Id> {
    pub winner: Id,
    pub rect: Rectangle,
    /// Overlays to remove now.
    pub remove: Vec<Id>,
    /// Overlays kept for a late `enter`: take their keyboard focus away and remove them when
    /// [`LATE_ENTER`] expires.
    pub keep: Vec<Id>,
    pub by_pointer: bool,
}

#[derive(Debug, Clone)]
struct Surface<Id> {
    id: Id,
    output: Option<String>,
    size: Option<Size>,
}

/// Placement state of one popup session.
#[derive(Debug, Clone)]
pub struct Placement<Id> {
    size: Size,
    surfaces: Vec<Surface<Id>>,
    winner: Option<Id>,
    rect: Option<Rectangle>,
    fallback_armed: bool,
    fallback_at: Option<Instant>,
    fallback_due: bool,
    interacted: bool,
    pending: Option<(Id, Point)>,
}

impl<Id: Copy + PartialEq> Placement<Id> {
    /// `size` is the persisted popup size.
    pub fn new(size: Size) -> Self {
        Self {
            size,
            surfaces: Vec::new(),
            winner: None,
            rect: None,
            fallback_armed: false,
            fallback_at: None,
            fallback_due: false,
            interacted: false,
            pending: None,
        }
    }

    /// An overlay was requested for `output`. `None` means the compositor chooses; the first such
    /// overlay hosts the centered fallback.
    pub fn requested(&mut self, id: Id, output: Option<String>) {
        self.surfaces.push(Surface {
            id,
            output,
            size: None,
        });
    }

    pub fn contains(&self, id: Id) -> bool {
        self.surfaces.iter().any(|s| s.id == id)
    }

    pub fn ids(&self) -> Vec<Id> {
        self.surfaces.iter().map(|s| s.id).collect()
    }

    pub fn winner(&self) -> Option<Id> {
        self.winner
    }

    pub fn rect(&self) -> Option<Rectangle> {
        self.rect
    }

    /// Every requested overlay is configured and the pointer position is still unknown: the
    /// moment to ask a compositor that only sends `enter` on motion to resend it.
    pub fn waiting_for_pointer(&self) -> bool {
        self.winner.is_none()
            && self.pending.is_none()
            && !self.surfaces.is_empty()
            && self.surfaces.iter().all(|s| s.size.is_some())
    }

    fn surface(&self, id: Id) -> Option<&Surface<Id>> {
        self.surfaces.iter().find(|s| s.id == id)
    }

    fn surface_size(&self, id: Id) -> Option<Size> {
        self.surface(id)?.size
    }

    /// An overlay was configured with its logical size. Returns `true` exactly once, for the
    /// first overlay: arm the [`FALLBACK_DELAY`] timer now. A pointer position that arrived
    /// before the size was known is placed here.
    pub fn opened(&mut self, id: Id, size: Size) -> (bool, Option<Placed<Id>>) {
        let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == id) else {
            return (false, None);
        };
        surface.size = Some(size);
        let arm = !self.fallback_armed;
        self.fallback_armed = true;
        let placed = match self.pending {
            Some((pending, point)) if pending == id && self.winner.is_none() => {
                self.pending = None;
                self.place_at(id, point)
            }
            _ if self.fallback_due && self.winner.is_none() => self.center_on_chosen(),
            _ => None,
        };
        (arm, placed)
    }

    /// The overlay changed size (output mode or scale change). Returns the moved panel.
    pub fn resized(&mut self, id: Id, size: Size) -> Option<Rectangle> {
        let surface = self.surfaces.iter_mut().find(|s| s.id == id)?;
        surface.size = Some(size);
        if self.winner != Some(id) {
            return None;
        }
        let rect = geometry::keep_on(self.rect?, size);
        self.rect = Some(rect);
        Some(rect)
    }

    /// The pointer was seen on overlay `id` at surface-local `point`.
    pub fn pointer(
        &mut self,
        id: Id,
        kind: PointerKind,
        point: Point,
        now: Instant,
    ) -> Option<Placed<Id>> {
        if !self.contains(id) {
            return None;
        }
        if self.winner.is_none() {
            if self.surface_size(id).is_none() {
                self.pending = Some((id, point));
                return None;
            }
            return self.place_at(id, point);
        }
        let late = self
            .fallback_at
            .is_some_and(|at| now.duration_since(at) <= LATE_ENTER);
        if kind == PointerKind::Entered && late && !self.interacted {
            self.surface_size(id)?;
            return self.place_at(id, point);
        }
        if kind == PointerKind::Pressed {
            self.interacted = true;
        }
        None
    }

    fn place_at(&mut self, id: Id, point: Point) -> Option<Placed<Id>> {
        let surface = self.surface_size(id)?;
        let rect = geometry::at_pointer(point, self.size, surface);
        self.winner = Some(id);
        self.rect = Some(rect);
        self.fallback_at = None;
        let remove = self.others(id);
        self.surfaces.retain(|s| s.id == id);
        Some(Placed {
            winner: id,
            rect,
            remove,
            keep: Vec::new(),
            by_pointer: true,
        })
    }

    /// The [`FALLBACK_DELAY`] timer fired: center the panel unless the pointer already won.
    pub fn fallback(&mut self) -> Fallback<Id> {
        if self.winner.is_some() {
            return Fallback::Nothing;
        }
        self.fallback_due = true;
        if !self.surfaces.iter().any(|s| s.output.is_none()) {
            return Fallback::NeedSurface;
        }
        match self.center_on_chosen() {
            Some(placed) => Fallback::Placed(placed),
            None => Fallback::Nothing,
        }
    }

    /// Centers the panel on the mapped overlay the compositor placed.
    fn center_on_chosen(&mut self) -> Option<Placed<Id>> {
        let target = self
            .surfaces
            .iter()
            .find(|s| s.output.is_none() && s.size.is_some())?;
        let (id, surface) = (target.id, target.size?);
        let rect = geometry::centered(self.size, surface);
        self.winner = Some(id);
        self.rect = Some(rect);
        self.fallback_due = false;
        self.fallback_at = Some(Instant::now());
        Some(Placed {
            winner: id,
            rect,
            remove: Vec::new(),
            keep: self.others(id),
            by_pointer: false,
        })
    }

    /// [`LATE_ENTER`] passed: the overlays kept for a late `enter` must go.
    pub fn late_enter_expired(&mut self) -> Vec<Id> {
        self.fallback_at = None;
        let Some(winner) = self.winner else {
            return Vec::new();
        };
        let remove = self.others(winner);
        self.surfaces.retain(|s| s.id == winner);
        remove
    }

    /// The user typed or clicked: the panel no longer moves to a late `enter`.
    pub fn interacted(&mut self) {
        self.interacted = true;
    }

    /// The user resized the panel.
    pub fn set_rect(&mut self, rect: Rectangle) {
        self.rect = Some(rect);
        self.interacted = true;
    }

    /// An overlay is gone (removed or its output disappeared).
    pub fn closed(&mut self, id: Id) {
        self.surfaces.retain(|s| s.id != id);
        if self.winner == Some(id) {
            self.winner = None;
            self.rect = None;
        }
    }

    fn others(&self, id: Id) -> Vec<Id> {
        self.surfaces
            .iter()
            .filter(|s| s.id != id)
            .map(|s| s.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FHD: Size = Size::new(1920.0, 1080.0);
    const PANEL: Size = Size::new(720.0, 320.0);

    fn two_outputs() -> Placement<u8> {
        let mut p = Placement::new(PANEL);
        p.requested(1, Some("DP-1".into()));
        p.requested(2, Some("DP-2".into()));
        p
    }

    #[test]
    fn first_pointer_wins_and_losers_go() {
        let mut p = two_outputs();
        let now = Instant::now();
        assert_eq!(p.opened(1, FHD), (true, None));
        assert_eq!(p.opened(2, FHD), (false, None));
        let placed = p
            .pointer(2, PointerKind::Entered, Point::new(500.0, 400.0), now)
            .expect("placed");
        assert_eq!(placed.winner, 2);
        assert_eq!(placed.rect.position(), Point::new(512.0, 412.0));
        assert_eq!(placed.remove, vec![1]);
        assert!(placed.by_pointer);
        assert_eq!(
            p.pointer(1, PointerKind::Entered, Point::ORIGIN, now),
            None,
            "a loser cannot win later"
        );
        assert_eq!(p.fallback(), Fallback::Nothing);
    }

    /// Two per-output overlays mapped, no pointer: the fallback overlay (3) is requested.
    fn fallen_back() -> Placement<u8> {
        let mut p = two_outputs();
        p.opened(1, FHD);
        p.opened(2, FHD);
        assert_eq!(p.fallback(), Fallback::NeedSurface);
        p.requested(3, None);
        let (_, placed) = p.opened(3, Size::new(1280.0, 720.0));
        let placed = placed.expect("centered once the fallback overlay opens");
        assert_eq!(placed.winner, 3);
        assert_eq!(placed.rect.position(), Point::new(280.0, 200.0));
        assert_eq!(placed.keep, vec![1, 2]);
        assert!(placed.remove.is_empty());
        assert!(!placed.by_pointer);
        p
    }

    #[test]
    fn fallback_centers_on_a_compositor_chosen_overlay() {
        let mut p = fallen_back();
        assert_eq!(p.late_enter_expired(), vec![1, 2]);
        assert_eq!(p.ids(), vec![3]);
    }

    #[test]
    fn single_output_uses_the_compositor_chosen_overlay_directly() {
        let mut p = Placement::new(PANEL);
        p.requested(7, None);
        p.opened(7, FHD);
        let Fallback::Placed(placed) = p.fallback() else {
            panic!("expected a placement");
        };
        assert_eq!(placed.winner, 7);
        assert_eq!(placed.rect.position(), Point::new(600.0, 380.0));
        assert!(placed.keep.is_empty());
    }

    #[test]
    fn pointer_while_the_fallback_overlay_maps_still_wins() {
        let mut p = two_outputs();
        p.opened(1, FHD);
        p.opened(2, FHD);
        assert_eq!(p.fallback(), Fallback::NeedSurface);
        p.requested(3, None);
        let placed = p
            .pointer(
                1,
                PointerKind::Entered,
                Point::new(10.0, 10.0),
                Instant::now(),
            )
            .expect("pointer wins");
        assert_eq!(placed.remove, vec![2, 3]);
        assert_eq!(
            p.opened(3, FHD).1,
            None,
            "the late fallback overlay is ignored"
        );
    }

    #[test]
    fn waits_for_the_pointer_once_everything_is_mapped() {
        let mut p = two_outputs();
        assert!(!p.waiting_for_pointer());
        p.opened(1, FHD);
        assert!(!p.waiting_for_pointer(), "output 2 not mapped yet");
        p.opened(2, FHD);
        assert!(p.waiting_for_pointer());
        p.pointer(2, PointerKind::Entered, Point::ORIGIN, Instant::now());
        assert!(!p.waiting_for_pointer());
    }

    #[test]
    fn late_enter_moves_the_panel_to_the_pointer_output() {
        let mut p = fallen_back();
        let moved = p
            .pointer(
                2,
                PointerKind::Entered,
                Point::new(580.0, 300.0),
                Instant::now() + Duration::from_millis(120),
            )
            .expect("late enter moves");
        assert_eq!(moved.winner, 2);
        assert_eq!(moved.rect.position(), Point::new(592.0, 312.0));
        assert_eq!(moved.remove, vec![1, 3]);
    }

    #[test]
    fn late_enter_is_ignored_after_the_window_or_input() {
        let mut p = fallen_back();
        let late = Instant::now() + LATE_ENTER + Duration::from_millis(50);
        assert_eq!(
            p.pointer(1, PointerKind::Entered, Point::ORIGIN, late),
            None
        );

        let mut p = fallen_back();
        p.interacted();
        let soon = Instant::now() + Duration::from_millis(10);
        assert_eq!(
            p.pointer(1, PointerKind::Entered, Point::ORIGIN, soon),
            None
        );

        let mut p = fallen_back();
        assert_eq!(
            p.pointer(1, PointerKind::Moved, Point::ORIGIN, soon),
            None,
            "only enter repositions"
        );
    }

    #[test]
    fn pointer_before_configure_is_placed_on_open() {
        let mut p = two_outputs();
        assert_eq!(
            p.pointer(
                1,
                PointerKind::Entered,
                Point::new(10.0, 10.0),
                Instant::now()
            ),
            None
        );
        let (arm, placed) = p.opened(1, FHD);
        assert!(arm);
        assert_eq!(
            placed.expect("placed").rect.position(),
            Point::new(22.0, 22.0)
        );
    }

    #[test]
    fn output_resize_keeps_panel_on_screen() {
        let mut p = two_outputs();
        p.opened(1, FHD);
        p.pointer(
            1,
            PointerKind::Entered,
            Point::new(1900.0, 1060.0),
            Instant::now(),
        );
        let rect = p.resized(1, Size::new(1280.0, 720.0)).expect("moved");
        assert_eq!(rect.position(), Point::new(560.0, 400.0));
        assert_eq!(p.resized(2, FHD), None);
    }

    #[test]
    fn closing_the_winner_resets() {
        let mut p = two_outputs();
        p.opened(1, FHD);
        p.pointer(1, PointerKind::Entered, Point::ORIGIN, Instant::now());
        p.closed(1);
        assert_eq!(p.winner(), None);
        assert!(p.ids().is_empty());
    }
}
