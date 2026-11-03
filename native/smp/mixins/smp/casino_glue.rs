// The casino in the server's game: its menus, and what its effects become
// for a stock client (smp_casino's `Fx`): menu items, sounds and titles
// through the console's `playsound` and `title`, the action bar, fireworks
// and particles over a winner, a line for everyone. glue.rs calls it from the
// command, menu and tick hooks with the SMP's state already locked; the
// console commands it builds are run by glue.rs once that lock is let go.
// Added to the game as `game::smp_casino_glue`.

use crate::game::{
    Game,
    native_server::NativeServerPlayer,
    smp_casino::{self, Casino, Color, Fx, Icon},
    smp_economy::{Economy, Out, Player},
};
use horizon_client::{
    items::ItemStack,
    lan::data::component_nbt,
    text::{self, Text},
};

/// The crash's boss bars: one a player (a bar's players can't be a list of
/// names, and tags don't reach a server's players), `smp:casino_crash_<i>`
/// for the name in `bars[i]`, and the colour they all have.
static BARS: std::sync::Mutex<(Vec<Option<String>>, &str)> =
    std::sync::Mutex::new((Vec::new(), "yellow"));
/// The most bars at once; each taken off at the first hide of a run,
/// whatever it held (a run before may have left one).
const MAX_BARS: usize = 32;
static CLEARED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn bar_id(i: usize) -> String {
    format!("smp:casino_crash_{i}")
}

fn color(c: Color) -> u32 {
    match c {
        Color::White => text::WHITE,
        Color::Gray => text::GRAY,
        Color::DarkGray => text::DARK_GRAY,
        Color::Gold => text::GOLD,
        Color::Yellow => text::YELLOW,
        Color::Green => text::GREEN,
        Color::Red => text::RED,
        Color::Aqua => text::AQUA,
    }
}

fn stack(icon: Icon) -> ItemStack {
    let mut stack = ItemStack {
        item: icon.item.into(),
        count: u32::from(icon.count.clamp(1, 99)),
        damage: 0,
        extra: None,
    };
    let title =
        serde_json::json!({ "text": icon.name, "italic": false, "color": icon.color.name() });
    stack.data_mut().custom_name = Some(component_nbt(&title));
    stack
}

fn items(casino: &Casino, economy: &Economy, uuid: u128, now: i64) -> Vec<Option<ItemStack>> {
    casino
        .layout(economy, uuid, now)
        .into_iter()
        .map(|c| c.map(stack))
        .collect()
}

/// Opens (again) the player's casino menu on its view: a new window, titled
/// for the game. False when it couldn't open.
fn open(game: &mut Game<'_>, casino: &mut Casino, economy: &Economy, uuid: u128, now: i64) -> bool {
    let Some(view) = casino.seats.get(&uuid).map(|s| s.view) else {
        return false;
    };
    let items = items(casino, economy, uuid, now);
    let title = Text::literal(view.title()).with_color(text::DARK_GRAY);
    match game.native_open_menu(uuid, smp_casino::ROWS, title, items) {
        Some(w) => {
            if let Some(s) = casino.seats.get_mut(&uuid) {
                s.window = Some(w);
            }
            true
        }
        None => {
            casino.seats.remove(&uuid);
            false
        }
    }
}

/// `/casino ...` (inside the player's command).
pub(in crate::game) fn command(
    game: &mut Game<'_>,
    casino: &mut Casino,
    economy: &mut Economy,
    me: &Player,
    now: i64,
    line: &str,
) -> Vec<Out> {
    let Some((mut out, view)) = casino.command(economy, me, now, line) else {
        return Vec::new();
    };
    if let Some(view) = view {
        casino.sit(me.uuid, &me.name, view);
        if !open(game, casino, economy, me.uuid, now) {
            out.push(Out::Error("The casino couldn't open.".into()));
        }
    }
    out
}

/// A click in a player's casino menu: the console commands its effects need,
/// for after the SMP's lock.
pub(in crate::game) fn clicked(
    game: &mut Game<'_>,
    casino: &mut Casino,
    economy: &mut Economy,
    uuid: u128,
    slot: i32,
    shift: bool,
) -> Vec<String> {
    let Ok(slot) = usize::try_from(slot) else {
        return Vec::new();
    };
    if slot >= smp_casino::SIZE {
        return Vec::new();
    }
    let now = chrono::Utc::now().timestamp_millis();
    economy.now = now;
    let fx = casino.click(economy, uuid, slot, shift, now);
    apply(game, casino, economy, fx, now)
}

/// The casino's share of the server's tick: frames, rounds that end, the
/// pot's float (once). The console commands, for after the lock.
pub(in crate::game) fn tick(
    game: &mut Game<'_>,
    casino: &mut Casino,
    economy: &mut Economy,
    now: i64,
    online: Option<&[u128]>,
) -> Vec<String> {
    let mut fx = casino.tick(economy, now);
    // Once a second: crash bets of players who left.
    if let Some(online) = online {
        fx.extend(casino.gone(economy, online, now));
    }
    if fx.is_empty() {
        return Vec::new();
    }
    apply(game, casino, economy, fx, now)
}

/// Carries out `fx`: menus and lines now, console commands returned.
fn apply(
    game: &mut Game<'_>,
    casino: &mut Casino,
    economy: &Economy,
    fx: Vec<Fx>,
    now: i64,
) -> Vec<String> {
    let mut commands = Vec::new();
    let mut players: Option<Vec<NativeServerPlayer>> = None;
    let mut shown = Vec::new();
    for f in fx {
        // Where a player is, asked once for the whole batch.
        let mut find = |uuid: u128, game: &Game<'_>| -> Option<NativeServerPlayer> {
            players
                .get_or_insert_with(|| game.native_server_players())
                .iter()
                .find(|p| p.uuid == uuid)
                .cloned()
        };
        match f {
            Fx::Out(Out::Tell(uuid, s)) => {
                game.native_tell(uuid, Text::literal(s).with_color(text::GOLD));
            }
            Fx::Out(Out::All(s)) => game.native_broadcast(Text::literal(s).with_color(text::GOLD)),
            Fx::Out(Out::Reply(s) | Out::Error(s)) => log::info!("[smp] casino: {s}"),
            Fx::Sound {
                to,
                sound,
                volume,
                pitch,
            } => {
                if let Some(p) = find(to, game) {
                    let [x, y, z] = p.position;
                    commands.push(format!(
                        "execute in {} run playsound {sound} master {} {x:.2} {y:.2} {z:.2} {volume} {pitch}",
                        p.dimension, p.name
                    ));
                }
            }
            Fx::Title {
                to,
                title,
                subtitle,
                color: c,
            } => {
                if let Some(p) = find(to, game) {
                    let line = |s: &str, bold: bool| {
                        serde_json::json!({ "text": s, "color": c.name(), "bold": bold })
                            .to_string()
                    };
                    commands.push(format!("title {} times 5 40 15", p.name));
                    commands.push(format!(
                        "title {} subtitle {}",
                        p.name,
                        line(&subtitle, false)
                    ));
                    commands.push(format!("title {} title {}", p.name, line(&title, true)));
                }
            }
            Fx::Bar {
                to,
                text: s,
                color: c,
            } => {
                game.native_action_bar(to, Text::literal(s).with_color(color(c)));
            }
            Fx::Fireworks(to) => {
                if let Some(p) = find(to, game) {
                    let [x, y, z] = p.position;
                    // High enough over the player that their blast (5 blocks) never hurts.
                    for (dx, dz, colours) in [
                        (-3.0, 0.0, "16766720,16733525"),
                        (3.0, 1.5, "5635925,16777045"),
                        (0.0, -3.0, "5636095,16733695"),
                    ] {
                        commands.push(format!(
                            "execute in {} run summon minecraft:firework_rocket {:.2} {:.2} {:.2} {{LifeTime:18,FireworksItem:{{id:\"minecraft:firework_rocket\",count:1,components:{{\"minecraft:fireworks\":{{flight_duration:1b,explosions:[{{shape:\"large_ball\",colors:[I;{colours}],has_trail:true,has_twinkle:true}}]}}}}}}}}",
                            p.dimension,
                            x + dx,
                            y + 9.0,
                            z + dz
                        ));
                    }
                    commands.push(format!(
                        "execute in {} run particle minecraft:totem_of_undying {x:.2} {:.2} {z:.2} 0.6 1 0.6 0.4 60 normal",
                        p.dimension,
                        y + 1.0
                    ));
                }
            }
            Fx::Announce(s) => {
                log::info!("[smp] casino: {s}");
                game.native_broadcast(
                    Text::literal(format!("[Casino] {s}")).with_color(text::GOLD),
                );
            }
            Fx::Show(uuid) => {
                // One picture per menu per batch.
                if !shown.contains(&uuid)
                    && casino.seats.get(&uuid).is_some_and(|s| s.window.is_some())
                {
                    shown.push(uuid);
                    let items = items(casino, economy, uuid, now);
                    game.native_set_menu_items(uuid, items);
                }
            }
            Fx::Open(uuid) => {
                shown.push(uuid);
                open(game, casino, economy, uuid, now);
            }
            Fx::Boss {
                text: line,
                color: c,
                value,
            } => {
                let mut bars = BARS.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(c) = c {
                    bars.1 = match c {
                        Color::Green => "green",
                        Color::Red => "red",
                        _ => "yellow",
                    };
                }
                let name = serde_json::json!({ "text": line, "color": "white" });
                for (i, _) in bars.0.iter().enumerate().filter(|(_, b)| b.is_some()) {
                    let id = bar_id(i);
                    commands.push(format!("bossbar set {id} name {name}"));
                    commands.push(format!("bossbar set {id} value {value}"));
                    if c.is_some() {
                        commands.push(format!("bossbar set {id} color {}", bars.1));
                    }
                }
            }
            Fx::BossPlayers(names) => {
                let mut bars = BARS.lock().unwrap_or_else(|e| e.into_inner());
                let colour = bars.1;
                for (i, bar) in bars.0.iter_mut().enumerate() {
                    if bar.as_ref().is_some_and(|n| !names.contains(n)) {
                        *bar = None;
                        commands.push(format!("bossbar remove {}", bar_id(i)));
                    }
                }
                for name in names {
                    if bars.0.iter().any(|b| b.as_ref() == Some(&name)) {
                        continue;
                    }
                    let i = match bars.0.iter().position(Option::is_none) {
                        Some(i) => i,
                        None if bars.0.len() < MAX_BARS => {
                            bars.0.push(None);
                            bars.0.len() - 1
                        }
                        None => continue,
                    };
                    let id = bar_id(i);
                    commands.push(format!("bossbar add {id} \"\""));
                    commands.push(format!("bossbar set {id} max 100"));
                    commands.push(format!("bossbar set {id} color {colour}"));
                    commands.push(format!("bossbar set {id} players {name}"));
                    bars.0[i] = Some(name);
                }
            }
            Fx::BossHide => {
                // Every bar taken off.
                let mut bars = BARS.lock().unwrap_or_else(|e| e.into_inner());
                let all = !CLEARED.swap(true, std::sync::atomic::Ordering::Relaxed);
                let n = if all { MAX_BARS } else { bars.0.len() };
                for i in 0..n {
                    if all || bars.0[i].is_some() {
                        commands.push(format!("bossbar remove {}", bar_id(i)));
                    }
                }
                bars.0.clear();
                bars.1 = "yellow";
            }
        }
    }
    commands
}
