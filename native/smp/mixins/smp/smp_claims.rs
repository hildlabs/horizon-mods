// Land claims: a player claims the chunk they stand in (`/claim`), all its
// height, and only they and the players they trust (`/trust`) break blocks
// or right click blocks there (doors, chests, levers, placing, buckets on a
// block), or hit its frames, armor stands and animals. Nothing flows, burns
// or is pushed into it from land of someone else, and explosions spare it.
// The chunks a player may hold grow with the time they played here.
// Operators of level 3 pass everywhere and may `/unclaim` any chunk. Kept in
// `claims.json`, written atomically. No game types. Added to the game as
// `game::smp_claims`; also a module of this crate, for its tests.

use super::smp_economy::{Out, Player};
use super::smp_places::Place;
use super::smp_store;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// `config.json`'s `claims` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// Chunks a player may claim, by hours played here: `[hours, chunks]`,
    /// the last line reached counts.
    pub chunks: Vec<[i64; 2]>,
    /// The entities only the owner and trusted players may hit in a claim.
    pub protected_entities: Vec<String>,
    /// Blocks around 0 0, in every dimension, where no one may claim (free
    /// for all); 0 for none. Claims already there are dropped at start.
    pub free_radius: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            chunks: vec![[0, 4], [5, 16], [20, 36], [100, 64]],
            protected_entities: PROTECTED
                .iter()
                .map(|&e| format!("minecraft:{e}"))
                .collect(),
            free_radius: 0,
        }
    }
}

/// Frames, stands, vehicles and the animals a player keeps: not monsters.
/// Points along a chunk's four edges, every `step` blocks, at height `y`:
/// where `/claim show` draws its border.
pub fn border(cx: i32, cz: i32, y: f64, step: usize) -> Vec<[f64; 3]> {
    let (x0, z0) = (f64::from(cx * 16), f64::from(cz * 16));
    let mut points = Vec::new();
    for i in (0..=16).step_by(step.max(1)) {
        let t = f64::from(i);
        points.push([x0 + t, y, z0]);
        points.push([x0 + t, y, z0 + 16.0]);
        if i != 0 && i != 16 {
            points.push([x0, y, z0 + t]);
            points.push([x0 + 16.0, y, z0 + t]);
        }
    }
    points
}

const PROTECTED: &[&str] = &[
    "item_frame",
    "glow_item_frame",
    "armor_stand",
    "painting",
    "leash_knot",
    "minecart",
    "chest_minecart",
    "furnace_minecart",
    "hopper_minecart",
    "tnt_minecart",
    "oak_boat",
    "spruce_boat",
    "birch_boat",
    "jungle_boat",
    "acacia_boat",
    "dark_oak_boat",
    "mangrove_boat",
    "cherry_boat",
    "pale_oak_boat",
    "bamboo_raft",
    "oak_chest_boat",
    "spruce_chest_boat",
    "birch_chest_boat",
    "jungle_chest_boat",
    "acacia_chest_boat",
    "dark_oak_chest_boat",
    "mangrove_chest_boat",
    "cherry_chest_boat",
    "pale_oak_chest_boat",
    "bamboo_chest_raft",
    "allay",
    "armadillo",
    "axolotl",
    "bee",
    "camel",
    "cat",
    "chicken",
    "cow",
    "donkey",
    "fox",
    "frog",
    "goat",
    "happy_ghast",
    "horse",
    "llama",
    "mooshroom",
    "mule",
    "ocelot",
    "panda",
    "parrot",
    "pig",
    "rabbit",
    "sheep",
    "sniffer",
    "snow_golem",
    "iron_golem",
    "copper_golem",
    "strider",
    "turtle",
    "villager",
    "wolf",
    "trader_llama",
    "wandering_trader",
];

/// `claims.json`.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Data {
    /// Claimed chunks, `<dimension> <chunk x> <chunk z>`, to their owner's UUID.
    chunks: BTreeMap<String, String>,
    /// The players each owner trusts, by UUID.
    trust: BTreeMap<String, BTreeSet<String>>,
}

pub struct Claims {
    pub config: Config,
    path: PathBuf,
    data: Data,
    changed: bool,
    /// `data.chunks` by dimension, then (chunk x, chunk z): asked on every
    /// fluid flow, so a lookup allocates nothing.
    owners: HashMap<String, HashMap<(i32, i32), u128>>,
    /// When each player was last told a chunk isn't theirs.
    told: HashMap<u128, i64>,
}

fn key(dimension: &str, cx: i32, cz: i32) -> String {
    format!("{dimension} {cx} {cz}")
}

fn chunk_of(at: &Place) -> (i32, i32) {
    ((at.x.floor() as i32) >> 4, (at.z.floor() as i32) >> 4)
}

impl Claims {
    /// `claims.json` in `dir`; an error when it exists but can't be read
    /// (never replaced by an empty one).
    pub fn open(dir: &Path, config: Config) -> Result<Self, String> {
        let path = dir.join("claims.json");
        let data: Data = smp_store::read_json(&path)?;
        let mut owners: HashMap<String, HashMap<(i32, i32), u128>> = HashMap::new();
        for (k, owner) in &data.chunks {
            let mut w = k.split(' ');
            let (Some(d), Some(x), Some(z), Some(u)) = (
                w.next(),
                w.next().and_then(|x| x.parse().ok()),
                w.next().and_then(|z| z.parse().ok()),
                smp_store::parse_uuid(owner),
            ) else {
                return Err(format!("{}: a bad claim `{k}`", path.display()));
            };
            owners.entry(d.to_owned()).or_default().insert((x, z), u);
        }
        Ok(Self {
            config,
            path,
            data,
            changed: false,
            owners,
            told: HashMap::new(),
        })
    }

    /// Whether a chunk touches the free zone around 0 0 (any dimension).
    fn in_free_zone(&self, cx: i32, cz: i32) -> bool {
        let r = self.config.free_radius;
        let near = |c: i32| i64::from(c) * 16 <= r && i64::from(c) * 16 + 15 >= -r;
        r > 0 && near(cx) && near(cz)
    }

    /// Drops the claims inside the free zone (a radius set after they were
    /// made): one line each, for the log.
    pub fn clear_free_zone(&mut self) -> Vec<String> {
        let inside: Vec<(String, i32, i32, u128)> = self
            .owners
            .iter()
            .flat_map(|(d, c)| {
                c.iter()
                    .filter(|((x, z), _)| self.in_free_zone(*x, *z))
                    .map(|((x, z), u)| (d.clone(), *x, *z, *u))
            })
            .collect();
        let mut lines = Vec::new();
        for (dimension, cx, cz, owner) in inside {
            if let Some(c) = self.owners.get_mut(&dimension) {
                c.remove(&(cx, cz));
            }
            self.data.chunks.remove(&key(&dimension, cx, cz));
            self.changed = true;
            lines.push(format!(
                "chunk {cx} {cz} (x {}, z {}) in {dimension} of {} is in the free zone: unclaimed",
                cx * 16,
                cz * 16,
                smp_store::uuid_string(owner)
            ));
        }
        lines.sort();
        lines
    }

    pub fn save(&mut self) -> Result<(), String> {
        if self.changed {
            smp_store::write_json(&self.path, &self.data)?;
            self.changed = false;
        }
        Ok(())
    }

    /// Who holds the chunk of this block, if anyone.
    pub fn owner(&self, dimension: &str, x: i32, z: i32) -> Option<u128> {
        self.chunk_owner(dimension, x >> 4, z >> 4)
    }

    fn chunk_owner(&self, dimension: &str, cx: i32, cz: i32) -> Option<u128> {
        self.owners.get(dimension)?.get(&(cx, cz)).copied()
    }

    /// Whether the world may change the block at `to` from `from` (a fluid,
    /// fire, a piston): yes, unless `to` is claimed by someone other than
    /// `from`'s owner or a player they trust.
    pub fn spread_allowed(&self, dimension: &str, from: [i32; 3], to: [i32; 3]) -> bool {
        if !self.config.enabled {
            return true;
        }
        let Some(owner) = self.owner(dimension, to[0], to[2]) else {
            return true;
        };
        match self.owner(dimension, from[0], from[2]) {
            Some(source) => source == owner || self.trusts(owner, source),
            None => false,
        }
    }

    /// Takes out of an explosion's blocks those in a claim: explosions
    /// spare claimed land, whoever lit them.
    pub fn spare(&self, dimension: &str, blocks: &mut Vec<[i32; 3]>) {
        if self.config.enabled && self.owners.get(dimension).is_some_and(|c| !c.is_empty()) {
            blocks.retain(|b| self.owner(dimension, b[0], b[2]).is_none());
        }
    }

    /// Whose land a block is to `who`: None when free, true when theirs or
    /// trusted to them, false when someone else's.
    pub fn relation(&self, dimension: &str, x: i32, z: i32, who: u128) -> Option<bool> {
        let owner = self.owner(dimension, x, z)?;
        Some(owner == who || self.trusts(owner, who))
    }

    /// Whether entities of type `kind` (`minecraft:cow`) are kept from
    /// players a claim's owner doesn't trust.
    pub fn protects(&self, kind: &str) -> bool {
        self.config.protected_entities.iter().any(|e| e == kind)
    }

    fn trusts(&self, owner: u128, who: u128) -> bool {
        self.data
            .trust
            .get(&smp_store::uuid_string(owner))
            .is_some_and(|t| t.contains(&smp_store::uuid_string(who)))
    }

    /// Whether `who` may break or use the block at `pos`. When not, the line
    /// to tell them: at most one each 2 s, so holding a click doesn't flood
    /// their chat.
    pub fn check(
        &mut self,
        who: &Player,
        dimension: &str,
        pos: [i32; 3],
        name_of: &dyn Fn(u128) -> String,
        now: i64,
    ) -> Result<(), Option<String>> {
        if !self.config.enabled || who.level >= 3 {
            return Ok(());
        }
        let Some(owner) = self.owner(dimension, pos[0], pos[2]) else {
            return Ok(());
        };
        if owner == who.uuid || self.trusts(owner, who.uuid) {
            return Ok(());
        }
        let last = self.told.insert(who.uuid, now);
        if last.is_some_and(|t| now - t < 2_000) {
            return Err(None);
        }
        Err(Some(format!("This land is claimed by {}.", name_of(owner))))
    }

    /// How many chunks `uuid` holds.
    pub fn held(&self, uuid: u128) -> usize {
        self.owners
            .values()
            .flat_map(|c| c.values())
            .filter(|&&u| u == uuid)
            .count()
    }

    /// How many chunks a player may hold after `played` seconds here.
    pub fn limit(&self, played: i64) -> usize {
        self.config
            .chunks
            .iter()
            .filter(|[h, _]| played / 3600 >= *h)
            .map(|[_, n]| (*n).max(0) as usize)
            .next_back()
            .unwrap_or(0)
    }

    /// The claims' commands; None for another command. `at` is where the
    /// player stands; `find` a player's UUID and name from a name (on or
    /// known); `name_of` a name from a UUID.
    pub fn command(
        &mut self,
        me: &Player,
        at: &Place,
        played: i64,
        find: &dyn Fn(&str) -> Option<(u128, String)>,
        name_of: &dyn Fn(u128) -> String,
        line: &str,
    ) -> Option<Vec<Out>> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let command = *words.first()?;
        if !COMMANDS.contains(&command) {
            return None;
        }
        if !self.config.enabled {
            return Some(vec![Out::Error("Claims are off on this server.".into())]);
        }
        let (cx, cz) = chunk_of(at);
        let here = at.dimension.as_str();
        let blocks = format!(
            "x {} to {}, z {} to {}",
            cx * 16,
            cx * 16 + 15,
            cz * 16,
            cz * 16 + 15
        );
        let me_key = smp_store::uuid_string(me.uuid);
        Some(match (command, &words[1..]) {
            ("claim", []) => match self.chunk_owner(here, cx, cz) {
                Some(u) if u == me.uuid => vec![Out::Error("This chunk is already yours.".into())],
                Some(u) => vec![Out::Error(format!("This chunk is {}'s.", name_of(u)))],
                None if self.in_free_zone(cx, cz) => vec![Out::Error(format!(
                    "No claims within {} blocks of 0 0: that land is free for all.",
                    self.config.free_radius
                ))],
                None => {
                    let (held, limit) = (self.held(me.uuid), self.limit(played));
                    if held >= limit {
                        vec![Out::Error(format!(
                            "You hold {held} of {limit} chunks: play longer for more, or /unclaim one."
                        ))]
                    } else {
                        self.data.chunks.insert(key(here, cx, cz), me_key);
                        let chunks = self.owners.entry(here.to_owned()).or_default();
                        chunks.insert((cx, cz), me.uuid);
                        self.changed = true;
                        vec![Out::Reply(format!(
                            "Claimed this chunk ({blocks}), {} of {limit}. /trust <player> lets a friend build.",
                            held + 1
                        ))]
                    }
                }
            },
            ("claim", ["info"]) => vec![Out::Reply(match self.chunk_owner(here, cx, cz) {
                Some(u) => format!("This chunk ({blocks}) is {}'s.", name_of(u)),
                None => format!("This chunk ({blocks}) is free: /claim it."),
            })],
            ("unclaim", []) => match self.chunk_owner(here, cx, cz) {
                Some(u) if u == me.uuid || me.level >= 3 => {
                    if let Some(chunks) = self.owners.get_mut(here) {
                        chunks.remove(&(cx, cz));
                    }
                    self.data.chunks.remove(&key(here, cx, cz));
                    self.changed = true;
                    vec![Out::Reply(format!("Unclaimed this chunk ({blocks})."))]
                }
                Some(u) => vec![Out::Error(format!("This chunk is {}'s.", name_of(u)))],
                None => vec![Out::Error("This chunk isn't claimed.".into())],
            },
            ("claims", []) => {
                let mine: Vec<String> = self
                    .data
                    .chunks
                    .iter()
                    .filter(|(_, o)| **o == me_key)
                    .map(|(k, _)| {
                        let w: Vec<&str> = k.split(' ').collect();
                        let n = |s: &str| s.parse::<i32>().unwrap_or(0) * 16;
                        format!(
                            "{} at x {} z {}",
                            w[0].trim_start_matches("minecraft:"),
                            n(w[1]),
                            n(w[2])
                        )
                    })
                    .collect();
                let trusted: Vec<String> = self
                    .data
                    .trust
                    .get(&me_key)
                    .into_iter()
                    .flatten()
                    .filter_map(|u| smp_store::parse_uuid(u))
                    .map(name_of)
                    .collect();
                vec![
                    Out::Reply(format!(
                        "Your chunks ({} of {}): {}",
                        mine.len(),
                        self.limit(played),
                        if mine.is_empty() {
                            "none, /claim where you stand".into()
                        } else {
                            mine.join("; ")
                        }
                    )),
                    Out::Reply(format!(
                        "Trusted: {}",
                        if trusted.is_empty() {
                            "nobody".into()
                        } else {
                            trusted.join(", ")
                        }
                    )),
                ]
            }
            ("trust", [name]) | ("untrust", [name]) => match find(name) {
                None => vec![Out::Error(format!(
                    "No player named {name} has played here."
                ))],
                Some((u, _)) if u == me.uuid => vec![Out::Error("That's you.".into())],
                Some((u, name)) => {
                    let set = self.data.trust.entry(me_key.clone()).or_default();
                    let changed = if command == "trust" {
                        set.insert(smp_store::uuid_string(u))
                    } else {
                        set.remove(&smp_store::uuid_string(u))
                    };
                    if set.is_empty() {
                        self.data.trust.remove(&me_key);
                    }
                    self.changed |= changed;
                    vec![Out::Reply(match (command, changed) {
                        ("trust", true) => {
                            format!("{name} may now build and open things on your land.")
                        }
                        ("trust", false) => format!("You already trust {name}."),
                        (_, true) => format!("{name} no longer may build on your land."),
                        _ => format!("You didn't trust {name}."),
                    })]
                }
            },
            _ => vec![Out::Error(
                "Usage: /claim, /claim info, /claim show, /unclaim, /claims, /trust <player>, /untrust <player>"
                    .into(),
            )],
        })
    }
}

/// The claims' commands.
pub const COMMANDS: [&str; 5] = ["claim", "unclaim", "claims", "trust", "untrust"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const A: u128 = 1;
    const B: u128 = 2;

    fn dir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("smp-claims-{}-{n}", std::process::id()))
    }
    fn at(x: f64, z: f64) -> Place {
        Place {
            dimension: "minecraft:overworld".into(),
            x,
            y: 64.0,
            z,
        }
    }
    fn player(uuid: u128, level: u8) -> Player {
        Player {
            uuid,
            name: if uuid == A { "Alex" } else { "Steve" }.into(),
            level,
        }
    }
    fn find(n: &str) -> Option<(u128, String)> {
        match n.to_ascii_lowercase().as_str() {
            "alex" => Some((A, "Alex".into())),
            "steve" => Some((B, "Steve".into())),
            _ => None,
        }
    }
    fn name_of(u: u128) -> String {
        if u == A { "Alex" } else { "Steve" }.into()
    }

    #[test]
    fn a_border_runs_along_the_chunks_edges() {
        let points = border(1, -1, 70.0, 2);
        // 9 points on each edge, the corners once.
        assert_eq!(points.len(), 32);
        assert!(points.contains(&[16.0, 70.0, -16.0]) && points.contains(&[32.0, 70.0, 0.0]));
        assert!(
            points
                .iter()
                .all(|p| p[0] == 16.0 || p[0] == 32.0 || p[2] == -16.0 || p[2] == 0.0)
        );
        let d = dir();
        let mut c = Claims::open(&d, Config::default()).unwrap();
        c.command(&player(A, 0), &at(8.0, 8.0), 0, &find, &name_of, "claim")
            .unwrap();
        let ow = "minecraft:overworld";
        assert_eq!(c.relation(ow, 3, 3, A), Some(true));
        assert_eq!(c.relation(ow, 3, 3, B), Some(false));
        assert_eq!(c.relation(ow, 20, 3, B), None);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn no_claims_in_the_free_zone_and_old_ones_are_dropped() {
        let d = dir();
        let mut c = Claims::open(&d, Config::default()).unwrap();
        let alex = player(A, 0);
        for (x, z) in [(8.0, 8.0), (-520.0, 8.0), (600.0, 600.0)] {
            c.command(&alex, &at(x, z), 1_000_000, &find, &name_of, "claim")
                .unwrap();
        }
        c.save().unwrap();
        let config = Config {
            free_radius: 500,
            ..Config::default()
        };
        let mut c = Claims::open(&d, config).unwrap();
        // Chunk 0 0 and chunk -33 0 (x -528 to -513: outside 500) stay apart.
        assert_eq!(c.clear_free_zone().len(), 1);
        let ow = "minecraft:overworld";
        assert_eq!(c.owner(ow, 8, 8), None);
        assert_eq!(c.owner(ow, -520, 8), Some(A));
        assert_eq!(c.owner(ow, 600, 600), Some(A));
        let out = c
            .command(
                &alex,
                &at(490.0, -490.0),
                1_000_000,
                &find,
                &name_of,
                "claim",
            )
            .unwrap();
        assert!(matches!(&out[0], Out::Error(e) if e.contains("free for all")));
        // The zone is the same in every dimension.
        let nether = Place {
            dimension: "minecraft:the_nether".into(),
            ..at(8.0, 8.0)
        };
        let out = c
            .command(&alex, &nether, 1_000_000, &find, &name_of, "claim")
            .unwrap();
        assert!(matches!(&out[0], Out::Error(e) if e.contains("free for all")));
        // Saved without them.
        c.save().unwrap();
        let again = Claims::open(&d, Config::default()).unwrap();
        assert_eq!(again.owner(ow, 8, 8), None);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_world_stays_out_of_a_claim() {
        let d = dir();
        let mut c = Claims::open(&d, Config::default()).unwrap();
        c.command(&player(A, 0), &at(8.0, 8.0), 0, &find, &name_of, "claim")
            .unwrap();
        let ow = "minecraft:overworld";
        // Into Alex's chunk from outside: refused; inside it and out of it: allowed.
        assert!(!c.spread_allowed(ow, [16, 64, 8], [15, 64, 8]));
        assert!(c.spread_allowed(ow, [14, 64, 8], [15, 64, 8]));
        assert!(c.spread_allowed(ow, [15, 64, 8], [16, 64, 8]));
        assert!(c.spread_allowed("minecraft:the_nether", [16, 64, 8], [15, 64, 8]));
        // From Steve's chunk next door: allowed once Alex trusts Steve.
        c.command(&player(B, 0), &at(24.0, 8.0), 0, &find, &name_of, "claim")
            .unwrap();
        assert!(!c.spread_allowed(ow, [16, 64, 8], [15, 64, 8]));
        c.command(
            &player(A, 0),
            &at(8.0, 8.0),
            0,
            &find,
            &name_of,
            "trust steve",
        )
        .unwrap();
        assert!(c.spread_allowed(ow, [16, 64, 8], [15, 64, 8]));
        assert!(!c.spread_allowed(ow, [15, 64, 8], [16, 64, 8]));
        // Explosions spare both claims, not the land around them.
        let mut blocks = vec![[0, 64, 0], [20, 64, 0], [40, 64, 0], [-1, 64, 0]];
        c.spare(ow, &mut blocks);
        assert_eq!(blocks, vec![[40, 64, 0], [-1, 64, 0]]);
        assert!(c.protects("minecraft:item_frame") && c.protects("minecraft:cow"));
        assert!(!c.protects("minecraft:zombie") && !c.protects("minecraft:player"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_claim_keeps_others_out_until_trusted() {
        let d = dir();
        let mut c = Claims::open(&d, Config::default()).unwrap();
        let out = c
            .command(&player(A, 0), &at(-3.5, 20.0), 0, &find, &name_of, "claim")
            .unwrap();
        assert!(
            matches!(&out[0], Out::Reply(s) if s.contains("x -16 to -1, z 16 to 31")),
            "{out:?}"
        );
        let ow = "minecraft:overworld";
        assert_eq!(
            c.check(&player(A, 0), ow, [-16, 0, 31], &name_of, 0),
            Ok(())
        );
        assert_eq!(
            c.check(&player(B, 0), ow, [-1, 100, 16], &name_of, 0),
            Err(Some("This land is claimed by Alex.".into()))
        );
        // Told once each 2 s; the next chunk and the Nether are free.
        assert_eq!(
            c.check(&player(B, 0), ow, [-1, 100, 16], &name_of, 1_000),
            Err(None)
        );
        assert_eq!(c.check(&player(B, 0), ow, [0, 64, 16], &name_of, 0), Ok(()));
        assert_eq!(
            c.check(
                &player(B, 0),
                "minecraft:the_nether",
                [-1, 64, 16],
                &name_of,
                0
            ),
            Ok(())
        );
        // Operators pass.
        assert_eq!(
            c.check(&player(B, 3), ow, [-1, 64, 16], &name_of, 0),
            Ok(())
        );
        c.command(
            &player(A, 0),
            &at(0.0, 0.0),
            0,
            &find,
            &name_of,
            "trust steve",
        )
        .unwrap();
        assert_eq!(
            c.check(&player(B, 0), ow, [-1, 64, 16], &name_of, 9_000),
            Ok(())
        );
        let out = c
            .command(
                &player(B, 0),
                &at(-3.5, 20.0),
                0,
                &find,
                &name_of,
                "unclaim",
            )
            .unwrap();
        assert!(
            matches!(&out[0], Out::Error(s) if s.contains("Alex's")),
            "trust isn't ownership"
        );
        c.save().unwrap();
        let mut again = Claims::open(&d, Config::default()).unwrap();
        assert_eq!(again.owner(ow, -1, 16), Some(A));
        again
            .command(
                &player(A, 0),
                &at(0.0, 0.0),
                0,
                &find,
                &name_of,
                "untrust Steve",
            )
            .unwrap();
        assert!(
            again
                .check(&player(B, 0), ow, [-1, 64, 16], &name_of, 20_000)
                .is_err()
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn chunks_grow_with_time_played() {
        let d = dir();
        let mut c = Claims::open(&d, Config::default()).unwrap();
        for x in 0..4 {
            let out = c
                .command(
                    &player(A, 0),
                    &at(x as f64 * 16.0, 0.0),
                    0,
                    &find,
                    &name_of,
                    "claim",
                )
                .unwrap();
            assert!(matches!(&out[0], Out::Reply(_)), "{out:?}");
        }
        let out = c
            .command(&player(A, 0), &at(64.0, 0.0), 0, &find, &name_of, "claim")
            .unwrap();
        assert!(
            matches!(&out[0], Out::Error(s) if s.contains("4 of 4")),
            "{out:?}"
        );
        let out = c
            .command(
                &player(A, 0),
                &at(64.0, 0.0),
                5 * 3600,
                &find,
                &name_of,
                "claim",
            )
            .unwrap();
        assert!(
            matches!(&out[0], Out::Reply(s) if s.contains("5 of 16")),
            "{out:?}"
        );
        let out = c
            .command(&player(B, 0), &at(0.0, 0.0), 0, &find, &name_of, "claim")
            .unwrap();
        assert_eq!(out, vec![Out::Error("This chunk is Alex's.".into())]);
        // An operator may free any chunk.
        let out = c
            .command(&player(B, 3), &at(0.0, 0.0), 0, &find, &name_of, "unclaim")
            .unwrap();
        assert!(matches!(&out[0], Out::Reply(_)));
        let out = c
            .command(&player(A, 0), &at(0.0, 0.0), 0, &find, &name_of, "claims")
            .unwrap();
        assert!(
            matches!(&out[0], Out::Reply(s) if s.starts_with("Your chunks (4 of 4)")),
            "{out:?}"
        );
        let _ = std::fs::remove_dir_all(d);
    }
}
