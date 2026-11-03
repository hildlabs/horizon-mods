// `/invsee <player>` and `/endersee <player>`: an operator of level 3 sees
// another player's inventory (main, hotbar, armour, off hand) or ender chest
// in a chest menu, on or off the server, and moves stacks in and out: a click
// on one of theirs takes it into the operator's inventory, a click on one of
// the operator's own puts it into theirs. One stack moves at a time, and only
// if it is still exactly what the menu showed (the game checks it and moves
// it whole, or nothing): no copy, no loss, whatever the player does
// meanwhile (moving items, leaving, joining, dying). Each move is in the
// audit log; closing the menu writes a one-line summary for the staff. The
// menu is read again once a second while it is open; nothing runs when no
// menu is open. No game types: the stacks are a type parameter, the game
// lends its inventories as a `World`. Added to the game as
// `game::smp_invsee`; also a module of this crate, for its tests.

use super::smp_economy::{Out, Player};
use std::collections::HashMap;

pub const COMMANDS: [&str; 2] = ["invsee", "endersee"];
/// The level that may look.
pub const LEVEL: u8 = 3;
pub const ROWS: u8 = 6;
pub const SIZE: usize = 54;
/// The bottom row's buttons.
const INFO: usize = 49;
const CLOSE: usize = 53;

/// Which inventory: the player's 41 slots (0..36 main, 36..40 armour feet to
/// head, 40 off hand), or the ender chest's 27.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Player,
    Ender,
}

/// A stack's item and count, for the log.
pub trait Stack: Clone + PartialEq {
    fn id(&self) -> String;
    fn count(&self) -> u32;
}

/// The game's inventories (the glue's, over the native helpers).
pub trait World<S> {
    /// A player's inventory and whether it is live (they are on): Err while
    /// they log in, are dead, or never played here.
    fn read(&self, uuid: u128, kind: Kind) -> Result<(Vec<Option<S>>, bool), String>;
    /// One stack from `from`'s slot, still exactly `expected`, into `to`'s
    /// inventory where it fits whole, or nothing moves.
    fn move_stack(
        &mut self,
        from: (u128, Kind, usize),
        expected: &S,
        to: (u128, Kind),
    ) -> Result<(), String>;
}

/// One operator's open menu.
#[derive(Clone, Debug)]
pub struct View<S> {
    pub target: u128,
    pub name: String,
    pub kind: Kind,
    pub window: Option<i32>,
    /// What the menu shows, by inventory slot.
    pub shown: Vec<Option<S>>,
    pub online: bool,
    /// Why the inventory can't be read now (logging in, dead...).
    pub busy: Option<String>,
    /// What moved: (taken from the player, item, count).
    pub moved: Vec<(bool, String, u32)>,
}

/// A cell of the menu.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell<S> {
    Empty,
    Stack(S),
    Filler,
    /// The player's name, on or off, and why it can't be read now.
    Info {
        name: String,
        online: bool,
        busy: Option<String>,
    },
    /// Above an armour or off-hand slot.
    Label(&'static str),
    Close,
}

/// A move the glue logs before it asks the game for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Move {
    pub op: u128,
    pub target: u128,
    pub kind: Kind,
    /// Taken from the player (true) or put into their inventory.
    pub take: bool,
    pub item: String,
    pub count: u32,
}

/// The menu slot of each inventory slot. Player: the main inventory in the
/// first three rows, the hotbar in the fourth (as the player sees it), the
/// armour head to feet then the off hand in the fifth.
pub fn menu_slot(kind: Kind, slot: usize) -> Option<usize> {
    match (kind, slot) {
        (Kind::Player, 9..36) => Some(slot - 9),
        (Kind::Player, 0..9) => Some(27 + slot),
        (Kind::Player, 36..40) => Some(36 + (39 - slot)),
        (Kind::Player, 40) => Some(41),
        (Kind::Ender, 0..27) => Some(slot),
        _ => None,
    }
}

/// The inventory slot a menu slot shows.
pub fn inventory_slot(kind: Kind, menu: usize) -> Option<usize> {
    let size = match kind {
        Kind::Player => 41,
        Kind::Ender => 27,
    };
    (0..size).find(|&s| menu_slot(kind, s) == Some(menu))
}

pub struct Invsee<S> {
    pub views: HashMap<u128, View<S>>,
}

impl<S> Default for Invsee<S> {
    fn default() -> Self {
        Self {
            views: HashMap::new(),
        }
    }
}

/// What a command asks to open: the player (UUID, real name) and which inventory.
pub type Open = Option<(u128, String, Kind)>;

impl<S: Stack> Invsee<S> {
    /// Runs `/invsee` or `/endersee`: the target named, to open at the next
    /// tick (the game's inventories aren't reachable inside a command).
    /// `find`: a player who has played here by name (UUID, real name).
    pub fn command(
        &mut self,
        me: &Player,
        find: &dyn Fn(&str) -> Option<(u128, String)>,
        line: &str,
    ) -> Option<(Vec<Out>, Open)> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let (&name, args) = words.split_first()?;
        let kind = match name {
            "invsee" => Kind::Player,
            "endersee" => Kind::Ender,
            _ => return None,
        };
        if me.level < LEVEL {
            return Some((
                vec![Out::Error(format!("Only operators may use /{name}."))],
                None,
            ));
        }
        let [who] = args else {
            return Some((vec![Out::Error(format!("Usage: /{name} <player>"))], None));
        };
        match find(who) {
            // Your own inventory would move stacks into itself; your own
            // ender chest is a second container, so an operator may open it.
            Some((uuid, _)) if uuid == me.uuid && kind == Kind::Player => {
                Some((vec![Out::Error("That is your own inventory.".into())], None))
            }
            Some((uuid, real)) => Some((Vec::new(), Some((uuid, real, kind)))),
            None => Some((
                vec![Out::Error(format!(
                    "No player named {who} has played here."
                ))],
                None,
            )),
        }
    }

    /// The view for `op` on `target`, read now (the glue opens the menu).
    pub fn open(
        &mut self,
        op: u128,
        target: u128,
        name: &str,
        kind: Kind,
        world: &dyn World<S>,
    ) -> Result<(), String> {
        let (shown, online) = world.read(target, kind)?;
        self.views.insert(
            op,
            View {
                target,
                name: name.to_owned(),
                kind,
                window: None,
                shown,
                online,
                busy: None,
                moved: Vec::new(),
            },
        );
        Ok(())
    }

    /// The menu's 54 cells for `op`.
    pub fn layout(&self, op: u128) -> Vec<Cell<S>> {
        let Some(v) = self.views.get(&op) else {
            return Vec::new();
        };
        let mut cells = vec![Cell::Empty; SIZE];
        let used = match v.kind {
            Kind::Player => 42,
            Kind::Ender => 27,
        };
        for c in cells.iter_mut().take(45).skip(used) {
            *c = Cell::Filler;
        }
        if v.kind == Kind::Player {
            cells[40] = Cell::Label("Off hand →");
        }
        for (slot, s) in v.shown.iter().enumerate() {
            if let (Some(m), Some(s)) = (menu_slot(v.kind, slot), s) {
                cells[m] = Cell::Stack(s.clone());
            }
        }
        for c in cells.iter_mut().skip(45) {
            *c = Cell::Filler;
        }
        cells[INFO] = Cell::Info {
            name: v.name.clone(),
            online: v.online,
            busy: v.busy.clone(),
        };
        cells[CLOSE] = Cell::Close;
        cells
    }

    /// Reads the target's inventory again: whether the menu changed.
    pub fn refresh(&mut self, op: u128, world: &dyn World<S>) -> bool {
        let Some(v) = self.views.get_mut(&op) else {
            return false;
        };
        let (shown, online, busy) = match world.read(v.target, v.kind) {
            Ok((s, on)) => (s, on, None),
            Err(e) => (v.shown.clone(), v.online, Some(e)),
        };
        let changed = shown != v.shown || online != v.online || busy != v.busy;
        (v.shown, v.online, v.busy) = (shown, online, busy);
        changed
    }

    /// A click on the menu's slot `menu`: the stack shown there into the
    /// operator's inventory. Ok(None): nothing to do; Ok(Some(close)) when
    /// the close button. `log` records the move first (the audit log); a
    /// move it refuses doesn't happen.
    pub fn click_menu(
        &mut self,
        op: u128,
        menu: usize,
        world: &mut dyn World<S>,
        log: &mut dyn FnMut(&Move) -> Result<(), String>,
    ) -> Result<Option<bool>, String> {
        let Some(v) = self.views.get(&op) else {
            return Ok(None);
        };
        if menu == CLOSE {
            return Ok(Some(true));
        }
        let Some(slot) = inventory_slot(v.kind, menu) else {
            return Ok(None);
        };
        let Some(stack) = v.shown.get(slot).cloned().flatten() else {
            return Ok(None);
        };
        let m = Move {
            op,
            target: v.target,
            kind: v.kind,
            take: true,
            item: stack.id(),
            count: stack.count(),
        };
        let r = self.apply(op, &m, world, log, |w| {
            w.move_stack((m.target, m.kind, slot), &stack, (op, Kind::Player))
        });
        self.refresh(op, world);
        r.map(|_| Some(false))
    }

    /// A click on the operator's own stack `stack` in inventory slot `slot`
    /// (0..36): into the player's inventory.
    pub fn click_own(
        &mut self,
        op: u128,
        slot: usize,
        stack: &S,
        world: &mut dyn World<S>,
        log: &mut dyn FnMut(&Move) -> Result<(), String>,
    ) -> Result<(), String> {
        let Some(v) = self.views.get(&op) else {
            return Ok(());
        };
        let m = Move {
            op,
            target: v.target,
            kind: v.kind,
            take: false,
            item: stack.id(),
            count: stack.count(),
        };
        let r = self.apply(op, &m, world, log, |w| {
            w.move_stack((op, Kind::Player, slot), stack, (m.target, m.kind))
        });
        self.refresh(op, world);
        r
    }

    fn apply(
        &mut self,
        op: u128,
        m: &Move,
        world: &mut dyn World<S>,
        log: &mut dyn FnMut(&Move) -> Result<(), String>,
        go: impl FnOnce(&mut dyn World<S>) -> Result<(), String>,
    ) -> Result<(), String> {
        log(m)?;
        go(world)?;
        if let Some(v) = self.views.get_mut(&op) {
            v.moved.push((m.take, m.item.clone(), m.count));
        }
        Ok(())
    }

    /// The operator's menu closed (or they left): the staff's one-line
    /// summary when something moved.
    pub fn close(&mut self, op: u128, op_name: &str) -> Option<String> {
        let v = self.views.remove(&op)?;
        if v.moved.is_empty() {
            return None;
        }
        let list = |take: bool| {
            let mut sums: Vec<(String, u32)> = Vec::new();
            for (t, item, n) in &v.moved {
                if *t == take {
                    let item = item.strip_prefix("minecraft:").unwrap_or(item).to_owned();
                    match sums.iter_mut().find(|(i, _)| *i == item) {
                        Some((_, c)) => *c += n,
                        None => sums.push((item, *n)),
                    }
                }
            }
            sums.iter()
                .map(|(i, n)| format!("{n} {i}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let what = match v.kind {
            Kind::Player => "inventory",
            Kind::Ender => "ender chest",
        };
        let (took, put) = (list(true), list(false));
        let mut parts = Vec::new();
        if !took.is_empty() {
            parts.push(format!("took {took}"));
        }
        if !put.is_empty() {
            parts.push(format!("put {put}"));
        }
        Some(format!(
            "{op_name} edited {}'s {what}{}: {}",
            v.name,
            if v.online { "" } else { " (offline)" },
            parts.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct S(&'static str, u32);
    impl Stack for S {
        fn id(&self) -> String {
            format!("minecraft:{}", self.0)
        }
        fn count(&self) -> u32 {
            self.1
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        On,
        Off,
        LoggingIn,
        Dead,
    }

    /// Two players: the operator (1, on) and Steve (2), whose inventories
    /// are live or a file by `state`; moves as the game's helper makes them.
    struct Fake {
        inv: HashMap<(u128, bool), Vec<Option<S>>>,
        state: State,
    }

    impl Fake {
        fn new() -> Self {
            let mut steve = vec![None; 41];
            steve[0] = Some(S("diamond", 5));
            steve[39] = Some(S("iron_helmet", 1));
            let mut ender = vec![None; 27];
            ender[3] = Some(S("emerald", 9));
            let mut inv = HashMap::new();
            inv.insert((1, false), vec![None; 41]);
            inv.insert((2, false), steve);
            inv.insert((2, true), ender);
            Self {
                inv,
                state: State::On,
            }
        }
        fn total(&self) -> u32 {
            self.inv.values().flatten().flatten().map(|s| s.1).sum()
        }
    }

    impl World<S> for Fake {
        fn read(&self, uuid: u128, kind: Kind) -> Result<(Vec<Option<S>>, bool), String> {
            if uuid == 2 {
                match self.state {
                    State::LoggingIn => return Err("the player is logging in".into()),
                    State::Dead => return Err("the player is dead".into()),
                    _ => {}
                }
            }
            let online = uuid == 1 || self.state == State::On;
            Ok((self.inv[&(uuid, kind == Kind::Ender)].clone(), online))
        }
        fn move_stack(
            &mut self,
            from: (u128, Kind, usize),
            expected: &S,
            to: (u128, Kind),
        ) -> Result<(), String> {
            self.read(from.0, from.1)?;
            self.read(to.0, to.1)?;
            let src = self.inv.get_mut(&(from.0, from.1 == Kind::Ender)).unwrap();
            if src[from.2].as_ref() != Some(expected) {
                return Err("that stack changed".into());
            }
            let s = src[from.2].take().unwrap();
            let dst = self.inv.get_mut(&(to.0, to.1 == Kind::Ender)).unwrap();
            let room = if to.1 == Kind::Player { 36 } else { 27 };
            match dst.iter_mut().take(room).find(|d| d.is_none()) {
                Some(d) => *d = Some(s),
                None => {
                    self.inv.get_mut(&(from.0, from.1 == Kind::Ender)).unwrap()[from.2] = Some(s);
                    return Err("no room".into());
                }
            }
            Ok(())
        }
    }

    fn op() -> Player {
        Player {
            uuid: 1,
            name: "Boss".into(),
            level: 4,
        }
    }

    fn opened(kind: Kind) -> (Invsee<S>, Fake, Vec<Move>) {
        let mut i = Invsee::default();
        let w = Fake::new();
        i.open(1, 2, "Steve", kind, &w).unwrap();
        (i, w, Vec::new())
    }

    #[test]
    fn only_operators_and_a_known_other_player() {
        let mut i: Invsee<S> = Invsee::default();
        let find = |n: &str| {
            (n == "Steve")
                .then(|| (2, "Steve".to_owned()))
                .or_else(|| (n == "Boss").then(|| (1, "Boss".into())))
        };
        let pleb = Player { level: 0, ..op() };
        assert_eq!(
            i.command(&pleb, &find, "invsee Steve").unwrap().0,
            [Out::Error("Only operators may use /invsee.".into())]
        );
        assert_eq!(
            i.command(&op(), &find, "invsee Boss").unwrap().0,
            [Out::Error("That is your own inventory.".into())]
        );
        assert_eq!(
            i.command(&op(), &find, "endersee Boss").unwrap().1,
            Some((1, "Boss".into(), Kind::Ender))
        );
        assert_eq!(
            i.command(&op(), &find, "endersee Nobody").unwrap().0,
            [Out::Error("No player named Nobody has played here.".into())]
        );
        assert_eq!(
            i.command(&op(), &find, "endersee Steve").unwrap().1,
            Some((2, "Steve".into(), Kind::Ender))
        );
        assert!(i.command(&op(), &find, "balance").is_none());
    }

    #[test]
    fn the_layout_shows_main_hotbar_armour_and_off_hand() {
        let (i, _, _) = opened(Kind::Player);
        let cells = i.layout(1);
        assert_eq!(
            cells[27],
            Cell::Stack(S("diamond", 5)),
            "the hotbar's first slot"
        );
        assert_eq!(
            cells[36],
            Cell::Stack(S("iron_helmet", 1)),
            "the head first"
        );
        assert_eq!(cells[44], Cell::Filler);
        assert!(matches!(&cells[INFO], Cell::Info { name, online: true, .. } if name == "Steve"));
        for s in 0..41 {
            assert_eq!(
                inventory_slot(Kind::Player, menu_slot(Kind::Player, s).unwrap()),
                Some(s)
            );
        }
    }

    #[test]
    fn taking_and_putting_move_one_stack_and_log_it_first() {
        let (mut i, mut w, mut log) = opened(Kind::Player);
        let before = w.total();
        let mut rec = |m: &Move| {
            log.push(m.clone());
            Ok(())
        };
        assert_eq!(i.click_menu(1, 27, &mut w, &mut rec), Ok(Some(false)));
        assert_eq!(w.inv[&(1, false)][0], Some(S("diamond", 5)));
        assert_eq!(i.layout(1)[27], Cell::Empty, "read again after the move");
        i.click_own(1, 0, &S("diamond", 5), &mut w, &mut rec)
            .unwrap();
        assert_eq!(w.inv[&(2, false)][0], Some(S("diamond", 5)));
        assert_eq!(w.total(), before);
        assert_eq!(log.len(), 2);
        assert!(
            log[0].take && !log[1].take && log[0].item == "minecraft:diamond" && log[0].count == 5
        );
        assert_eq!(
            i.close(1, "Boss").as_deref(),
            Some("Boss edited Steve's inventory: took 5 diamond; put 5 diamond")
        );
        assert!(i.views.is_empty());
    }

    #[test]
    fn a_stack_the_player_moved_meanwhile_is_not_copied() {
        let (mut i, mut w, _) = opened(Kind::Player);
        // Steve moves his diamonds after the menu was read.
        let d = w.inv.get_mut(&(2, false)).unwrap()[0].take();
        w.inv.get_mut(&(2, false)).unwrap()[5] = d;
        let before = w.total();
        assert_eq!(
            i.click_menu(1, 27, &mut w, &mut |_| Ok(())),
            Err("that stack changed".into())
        );
        assert_eq!(w.total(), before);
        assert!(w.inv[&(1, false)].iter().all(Option::is_none));
        // The menu shows where they are now.
        assert_eq!(i.layout(1)[32], Cell::Stack(S("diamond", 5)));
    }

    #[test]
    fn leaving_joining_and_dying_while_the_menu_is_open() {
        let (mut i, mut w, _) = opened(Kind::Ender);
        let before = w.total();
        // He leaves: his file, the same items, offline in the menu.
        w.state = State::Off;
        assert!(i.refresh(1, &w));
        assert!(matches!(
            &i.layout(1)[INFO],
            Cell::Info { online: false, .. }
        ));
        assert_eq!(i.click_menu(1, 3, &mut w, &mut |_| Ok(())), Ok(Some(false)));
        assert_eq!(w.inv[&(1, false)][0], Some(S("emerald", 9)));
        // He is logging in: nothing moves, the menu says why.
        w.state = State::LoggingIn;
        assert_eq!(
            i.click_own(1, 0, &S("emerald", 9), &mut w, &mut |_| Ok(())),
            Err("the player is logging in".into())
        );
        assert!(
            matches!(&i.layout(1)[INFO], Cell::Info { busy: Some(b), .. } if b.contains("logging in"))
        );
        // Dead: the same.
        w.state = State::Dead;
        assert!(
            i.click_own(1, 0, &S("emerald", 9), &mut w, &mut |_| Ok(()))
                .is_err()
        );
        // Back on: the move goes through live.
        w.state = State::On;
        i.click_own(1, 0, &S("emerald", 9), &mut w, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(w.total(), before);
        assert_eq!(
            i.close(1, "Boss").as_deref(),
            Some("Boss edited Steve's ender chest: took 9 emerald; put 9 emerald")
        );
    }

    #[test]
    fn a_move_the_log_refuses_does_not_happen_and_closing_with_nothing_moved_says_nothing() {
        let (mut i, mut w, _) = opened(Kind::Player);
        let before = w.inv.clone();
        assert_eq!(
            i.click_menu(1, 27, &mut w, &mut |_| Err("audit.log: disk full".into())),
            Err("audit.log: disk full".into())
        );
        assert_eq!(w.inv, before);
        assert_eq!(
            i.click_menu(1, CLOSE, &mut w, &mut |_| Ok(())),
            Ok(Some(true))
        );
        assert_eq!(i.close(1, "Boss"), None);
    }
}
