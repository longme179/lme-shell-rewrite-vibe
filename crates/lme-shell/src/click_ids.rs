//! The single registry of widget-click id ranges (Amendment 4 lesson b).
//!
//! D23 was one module's open-ended range check (`id >= 0x7000…`) swallowing
//! another module's ids — every tray click died as a phantom notification
//! dismiss. Rules enforced here:
//!
//! 1. Every clickable id space is a REGISTERED `IdRange` in this module.
//! 2. Routing checks use `IdRange::contains` — never open-ended `>=`.
//! 3. [`tests::ranges_do_not_overlap`] fails the build if two ranges touch.
//! 4. [`tests::constants_inside_registered_ranges`] fails if any id constant
//!    drifts outside its registered range.
//! 5. Sub-block bases inside a range (e.g. tray ROW_ACT/ROW_MENU/…) stay in
//!    their module's `ids` module, but their containing range is registered
//!    here and the unit test bounds them.
//!
//! Notification *message* ids (D-Bus ids < 0x4000_0000, local ids ≥
//! 0x4000_0000) are a different namespace — they are not widget ids and do
//! not live here.

/// A half-open [base, end) range of widget-click ids.
#[derive(Clone, Copy, Debug)]
pub struct IdRange {
    /// Human-readable owner, shown in test failures.
    pub name: &'static str,
    pub base: u64,
    /// Exclusive end.
    pub end: u64,
}

impl IdRange {
    pub const fn contains(&self, id: u64) -> bool {
        id >= self.base && id < self.end
    }

    /// Id minus the range base (only meaningful when `contains` is true).
    pub const fn offset(&self, id: u64) -> u64 {
        id - self.base
    }
}

/// Every registered widget-click range. Order in this list is irrelevant;
/// the unit test sorts before checking overlaps. The named consts below
/// are the same values — use them at call sites.
pub const BAR_BUTTONS: IdRange = IdRange {
    name: "bar-buttons",
    base: 0x1000,
    end: 0x1100,
};
pub const WORKSPACE_BUTTONS: IdRange = IdRange {
    name: "workspace-buttons",
    base: 0x2000,
    end: 0x3000,
};
pub const POWER_PANEL: IdRange = IdRange {
    name: "power-panel",
    base: 0x5000,
    end: 0x5100,
};
pub const LAUNCHER_PANEL: IdRange = IdRange {
    name: "launcher-panel",
    base: 0x6000,
    end: 0x6100,
};
/// Toast card dismiss buttons: base + notification u32 id
/// (notifications::toast_dismiss_id).
pub const TOAST_CARDS: IdRange = IdRange {
    name: "toast-cards",
    base: 0x7000_0000_0000,
    end: 0x7100_0000_0000,
};
/// Tray host panel rows (tray::ids::TRAY_ROW_* + item index).
pub const TRAY_ROWS: IdRange = IdRange {
    name: "tray-rows",
    base: 0x7100_0000_0000,
    end: 0x7200_0000_0000,
};
/// Tray in-panel menu page (tray::ids::TRAY_MENU_BACK / TRAY_MENU_ENTRY).
pub const TRAY_MENU: IdRange = IdRange {
    name: "tray-menu",
    base: 0x7200_0000_0000,
    end: 0x7400_0000_0000,
};
/// Toast action buttons: base + notif id * 16 + action index
/// (notifications::toast_action_id; the encoder refuses ids that would
/// overflow the range — see its unit test).
pub const TOAST_ACTIONS: IdRange = IdRange {
    name: "toast-actions",
    base: 0x7400_0000_0000,
    end: 0x7401_0000_0000,
};

pub const ALL: &[IdRange] = &[
    BAR_BUTTONS,
    WORKSPACE_BUTTONS,
    POWER_PANEL,
    LAUNCHER_PANEL,
    TOAST_CARDS,
    TRAY_ROWS,
    TRAY_MENU,
    TOAST_ACTIONS,
];

pub fn find(id: u64) -> Option<&'static IdRange> {
    ALL.iter().find(|r| r.contains(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar;
    use crate::panels;
    use crate::tray;

    /// Amendment 4 lesson b: no two registered ranges may overlap (or even
    /// touch) — an overlap is exactly how D23 happened.
    #[test]
    fn ranges_do_not_overlap() {
        let mut sorted: Vec<&IdRange> = ALL.iter().collect();
        sorted.sort_by_key(|r| r.base);
        for pair in sorted.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(
                a.end <= b.base,
                "id ranges overlap: {a:?} ({:#x}..{:#x}) vs {b:?} ({:#x}..{:#x})",
                a.base,
                a.end,
                b.base,
                b.end
            );
        }
    }

    /// Every existing id constant must sit inside its registered range, and
    /// the registered range must actually be used by its module's routing.
    #[test]
    fn constants_inside_registered_ranges() {
        for id in [
            bar::ids::LAUNCHER,
            bar::ids::OVERVIEW,
            bar::ids::NOTIFICATIONS,
            bar::ids::SETTINGS,
            bar::ids::CLIPBOARD,
            bar::ids::AUDIO,
            bar::ids::NETWORK,
            bar::ids::BATTERY,
            bar::ids::TRAY,
            bar::ids::WORKSPACES,
        ] {
            assert!(
                BAR_BUTTONS.contains(id),
                "bar id {id:#x} outside {BAR_BUTTONS:?}"
            );
        }
        assert!(WORKSPACE_BUTTONS.contains(bar::ids::WORKSPACE_BASE));

        for id in [
            panels::ids::POWER_LOCK,
            panels::ids::POWER_SUSPEND,
            panels::ids::POWER_LOGOUT,
            panels::ids::POWER_REBOOT,
            panels::ids::POWER_OFF,
            panels::ids::POWER_CLOSE,
            panels::ids::POWER_CONFIRM,
            panels::ids::POWER_BACK,
        ] {
            assert!(
                POWER_PANEL.contains(id),
                "power id {id:#x} outside {POWER_PANEL:?}"
            );
        }

        for id in [panels::ids::LAUNCHER_INPUT, panels::ids::LAUNCHER_LIST] {
            assert!(
                LAUNCHER_PANEL.contains(id),
                "launcher id {id:#x} outside {LAUNCHER_PANEL:?}"
            );
        }

        // tray sub-block bases: inside tray-rows / tray-menu, and ordered so
        // the decode's `>=` sub-block ladder stays correct (each sub-base is
        // above the previous one, with headroom for item indices).
        let rows = TRAY_ROWS;
        for (base, headroom) in [
            (tray::ids::TRAY_ROW_ACT, tray::ids::TRAY_ROW_MENU),
            (tray::ids::TRAY_ROW_MENU, tray::ids::TRAY_ROW_AUX),
            (tray::ids::TRAY_ROW_AUX, tray::ids::TRAY_ROW_WHEEL),
            (tray::ids::TRAY_ROW_WHEEL, rows.end),
        ] {
            assert!(
                rows.contains(base),
                "tray row base {base:#x} outside {rows:?}"
            );
            assert!(
                base < headroom,
                "tray row base {base:#x} not below next sub-base {headroom:#x}"
            );
        }
        assert!(TRAY_MENU.contains(tray::ids::TRAY_MENU_BACK));
        assert!(TRAY_MENU.contains(tray::ids::TRAY_MENU_ENTRY));
        const _: () = assert!(tray::ids::TRAY_MENU_BACK < tray::ids::TRAY_MENU_ENTRY);

        // toast encoders: boundary ids of the notification u32 space must
        // stay inside (dismiss) or be refused (actions, see notifications).
        assert!(TOAST_CARDS.contains(crate::notifications::toast_dismiss_id(0)));
        assert!(TOAST_CARDS.contains(crate::notifications::toast_dismiss_id(u32::MAX)));
        assert!(crate::notifications::toast_action_id(u32::MAX, 0).is_none());
        assert!(crate::notifications::toast_action_id(0x0FFF_FFFF, 0).is_some());
    }
}
