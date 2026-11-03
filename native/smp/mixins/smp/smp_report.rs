// The economy in the open: `/eco stats` (the money supply and where it
// sits, what the server made and destroyed today, how evenly money is
// spread), a report to everyone at 00:00 UTC on the day
// before, and `web/economy.json` for the site's /hub (written once a minute,
// off the tick thread, by the glue). One snapshot a day is kept (60 days) in
// `economy.json` (`ext.report`) for the curves. Reading only: this part
// moves no money. No game types. Added to the game as `game::smp_report`;
// also a module of this crate, for its tests.

use super::smp_economy::{DAY_MS, Economy, Out};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// `config.json`'s `report`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The 00:00 UTC report in everyone's chat.
    pub daily_report: bool,
    /// `web/economy.json` once a minute.
    pub web_json: bool,
    pub history_days: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daily_report: true,
            web_json: true,
            history_days: 60,
        }
    }
}

/// One day, as the report keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Day {
    /// Days since 1970 (UTC).
    pub day: i64,
    /// All the money there is, at the end of the day.
    pub supply: i64,
    /// Players' money: wallets, money held (bids, stakes).
    pub players: i64,
    /// Made and destroyed by the server that day.
    pub minted: i64,
    pub burned: i64,
    /// Over players with money: Gini (0 to 1, in thousandths) and the
    /// richest ten's share (percent).
    pub gini_milli: i64,
    pub top10_percent: i64,
}

/// `ext.report`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub days: Vec<Day>,
    /// The last day reported.
    pub day: i64,
}

pub struct Report {
    pub config: Config,
    pub state: State,
    dirty: bool,
    minute: i64,
}

/// Pools that aren't players' money: the casino's pot.
const HOUSE_POOLS: [&str; 1] = ["casino"];

/// Gini (thousandths) and the top ten's share (percent) of `wealth`.
pub fn spread(mut wealth: Vec<i64>) -> (i64, i64) {
    wealth.retain(|w| *w > 0);
    wealth.sort_unstable();
    let n = wealth.len() as i128;
    let total: i128 = wealth.iter().map(|w| i128::from(*w)).sum();
    if total == 0 {
        return (0, 0);
    }
    let weighted: i128 = wealth
        .iter()
        .enumerate()
        .map(|(i, w)| (2 * i as i128 - n + 1) * i128::from(*w))
        .sum();
    let top: i128 = wealth.iter().rev().take(10).map(|w| i128::from(*w)).sum();
    (
        (weighted * 1000 / (n * total)) as i64,
        (top * 100 / total) as i64,
    )
}

fn sum(m: &BTreeMap<String, i64>) -> i64 {
    m.values().sum()
}

/// `market sale 60,000 coins, daily reward 20,000 coins` (largest first).
fn list(e: &Economy, m: &BTreeMap<String, i64>, n: usize) -> String {
    let mut v: Vec<(&String, &i64)> = m.iter().filter(|(_, a)| **a > 0).collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let parts: Vec<String> = v
        .iter()
        .take(n)
        .map(|(k, a)| format!("{} {}", label(k), e.money(**a)))
        .collect();
    if parts.is_empty() {
        "nothing".into()
    } else {
        parts.join(", ")
    }
}

fn label(kind: &str) -> String {
    match kind {
        "sell" => "market sales".into(),
        "buy" => "market purchases".into(),
        k => k.replace('_', " "),
    }
}

impl Report {
    pub fn open(economy: &Economy, config: Config) -> Self {
        let mut state = economy
            .ext("report")
            .and_then(|v| serde_json::from_value::<State>(v.clone()).ok())
            .unwrap_or_default();
        if state.day == 0 {
            state.day = economy.now.div_euclid(DAY_MS);
        }
        Self {
            config,
            state,
            dirty: false,
            minute: 0,
        }
    }

    pub fn store(&mut self, economy: &mut Economy) {
        if self.dirty {
            economy.set_ext(
                "report",
                serde_json::to_value(&self.state).unwrap_or_default(),
            );
            self.dirty = false;
        }
    }

    /// The day's numbers now.
    pub fn snapshot(economy: &mut Economy, day: i64) -> Day {
        let accounts = economy.accounts();
        let wallets: i64 = accounts.iter().map(|a| a.2).sum();
        let pools: i64 = economy.pools().values().sum();
        let house: i64 = HOUSE_POOLS.iter().map(|p| economy.pool(p)).sum();
        let supply = wallets + economy.held_total() + pools;
        let (gini_milli, top10_percent) = spread(accounts.iter().map(|a| a.2).collect());
        let today = economy.now.div_euclid(DAY_MS);
        let f = economy.flows();
        let (minted, burned) = if day == today {
            (sum(&f.minted), sum(&f.burned))
        } else if day + 1 == today {
            (sum(&f.minted_yesterday), sum(&f.burned_yesterday))
        } else {
            (0, 0)
        };
        Day {
            day,
            supply,
            players: supply - house,
            minted,
            burned,
            gini_milli,
            top10_percent,
        }
    }

    /// Runs `/eco stats` (or `/money stats`); None for another command.
    pub fn command(&self, economy: &mut Economy, line: &str) -> Option<Vec<Out>> {
        let words: Vec<&str> = line.split_whitespace().collect();
        if !matches!(words.as_slice(), ["eco" | "money", "stats"]) {
            return None;
        }
        let today = economy.now.div_euclid(DAY_MS);
        let d = Self::snapshot(economy, today);
        let since = self.state.days.last().map(|y| d.supply - y.supply);
        let pools: Vec<String> = economy
            .pools()
            .iter()
            .filter(|(k, v)| **v > 0 && !HOUSE_POOLS.contains(&k.as_str()))
            .map(|(k, v)| format!("{} {}", k.replace('_', " "), economy.money(*v)))
            .collect();
        let flows = economy.flows().clone();
        Some(vec![
            Out::Reply(format!(
                "Money supply: {}{}. Players hold {} (of it in pools: {}), the casino's pot {}.",
                economy.money(d.supply),
                since.map_or(String::new(), |s| format!(
                    " ({}{} since midnight)",
                    if s >= 0 { "+" } else { "-" },
                    economy.money(s.abs())
                )),
                economy.money(d.players),
                if pools.is_empty() {
                    "none".into()
                } else {
                    pools.join(", ")
                },
                economy.money(d.supply - d.players)
            )),
            Out::Reply(format!(
                "Today the server made {} ({}) and destroyed {} ({}).",
                economy.money(sum(&flows.minted)),
                list(economy, &flows.minted, 3),
                economy.money(sum(&flows.burned)),
                list(economy, &flows.burned, 3)
            )),
            Out::Reply(format!(
                "Spread: Gini {}.{:03} (0 equal, 1 one player has all), the richest ten hold {}%.",
                d.gini_milli / 1000,
                d.gini_milli % 1000,
                d.top10_percent
            )),
        ])
    }

    /// Whether `tick` has work at `now` (a new minute or day).
    pub fn due(&self, now: i64) -> bool {
        now.div_euclid(60_000) != self.minute || now.div_euclid(DAY_MS) > self.state.day
    }

    /// Once a second from the glue. At the day's turn: the day before is
    /// kept and, if on, told to everyone. Returns the lines, and whether
    /// `web/economy.json` is due (once a minute).
    pub fn tick(&mut self, economy: &mut Economy, now: i64) -> (Vec<Out>, bool) {
        let minute = now.div_euclid(60_000);
        let web = self.config.web_json && minute != self.minute;
        self.minute = minute;
        let today = now.div_euclid(DAY_MS);
        if today <= self.state.day {
            return (Vec::new(), web);
        }
        let yesterday = today - 1;
        self.state.day = today;
        self.dirty = true;
        // The supply now is yesterday's end, give or take a second.
        let d = Self::snapshot(economy, yesterday);
        let before = self
            .state
            .days
            .last()
            .filter(|p| p.day + 1 == yesterday)
            .map(|p| p.supply);
        self.state.days.push(d.clone());
        let keep = self.config.history_days.max(1);
        if self.state.days.len() > keep {
            let extra = self.state.days.len() - keep;
            self.state.days.drain(..extra);
        }
        let mut out = Vec::new();
        if self.config.daily_report {
            out.push(Out::All(self.daily_line(economy, &d, before)));
        }
        (out, web)
    }

    fn daily_line(&self, economy: &Economy, d: &Day, before: Option<i64>) -> String {
        format!(
            "Economy yesterday: money supply {}{}; the server made {} and destroyed {}. /eco stats",
            economy.money(d.supply),
            before.map_or(String::new(), |b| {
                let c = d.supply - b;
                format!(
                    " ({}{})",
                    if c >= 0 { "+" } else { "-" },
                    economy.money(c.abs())
                )
            }),
            economy.money(d.minted),
            economy.money(d.burned)
        )
    }

    /// `web/economy.json`: aggregates and the days kept.
    pub fn web(&self, economy: &mut Economy, now: i64) -> Value {
        let today = now.div_euclid(DAY_MS);
        let d = Self::snapshot(economy, today);
        let flows = economy.flows().clone();
        let pools: BTreeMap<String, i64> = economy.pools().clone();
        let daily = self.state.days.last().map(|y| {
            self.daily_line(
                economy,
                y,
                self.state
                    .days
                    .iter()
                    .rev()
                    .nth(1)
                    .filter(|p| p.day + 1 == y.day)
                    .map(|p| p.supply),
            )
        });
        json!({
            "schema": 1,
            "generated": now,
            "currency": economy.config.currency,
            "today": d,
            "pools": pools,
            "minted_today": flows.minted,
            "burned_today": flows.burned,
            "days": self.state.days,
            "daily_report": daily,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smp_economy::Config as EcoConfig;

    const A: u128 = 1;
    const B: u128 = 2;
    const T0: i64 = 1_793_577_600_000; // a midnight, UTC

    fn setup(name: &str) -> (Economy, Report) {
        let dir = std::env::temp_dir().join(format!("smp-report-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut e = Economy::open(&dir, EcoConfig::default()).unwrap();
        e.now = T0 + 1000;
        e.seen(A, "Alex");
        e.seen(B, "Bo");
        let r = Report::open(&e, Config::default());
        (e, r)
    }

    #[test]
    fn spread_measures_inequality() {
        assert_eq!(spread(vec![10, 10, 10, 10]), (0, 100));
        let (g, _) = spread(vec![0, 0, 0, 100]);
        assert_eq!(g, 0, "only players with money count");
        let (g, top) = spread((1..=20).map(|i| if i == 20 { 1_000 } else { 1 }).collect());
        assert!(g > 900, "{g}");
        assert_eq!(top, 99);
    }

    #[test]
    fn stats_show_what_was_made_and_destroyed_today() {
        let (mut e, r) = setup("stats");
        e.adjust("sell 20 minecraft:wheat", A, 600, "").unwrap();
        e.adjust("daily_reward", B, 50, "").unwrap();
        e.adjust("buy 5 minecraft:torch", A, -100, "").unwrap();
        e.to_pool("ah_bid", A, "auction", 25, "").unwrap();
        let out = r.command(&mut e, "eco stats").unwrap();
        let text: Vec<String> = out
            .iter()
            .map(|o| match o {
                Out::Reply(s) => s.clone(),
                o => panic!("{o:?}"),
            })
            .collect();
        assert_eq!(
            text[0],
            "Money supply: 550 coins. Players hold 550 coins (of it in pools: auction 25 coins), the casino's pot 0 coins."
        );
        assert_eq!(
            text[1],
            "Today the server made 650 coins (market sales 600 coins, daily reward 50 coins) and destroyed 100 coins (market purchases 100 coins)."
        );
        assert!(!text.iter().any(|l| l.contains("Treasury")), "{text:?}");
        assert!(r.command(&mut e, "eco give A 5").is_none());
    }

    #[test]
    fn midnight_reports_the_day_before_once_and_keeps_it_through_a_restart() {
        let (mut e, mut r) = setup("daily");
        e.adjust("sell 1 minecraft:diamond", A, 100, "").unwrap();
        let (out, web) = r.tick(&mut e, T0 + 2000);
        assert!(out.is_empty() && web, "same day; the JSON's first minute");
        assert!(!r.tick(&mut e, T0 + 3000).1, "once a minute");
        e.now = T0 + DAY_MS + 500;
        let now = e.now;
        let (out, _) = r.tick(&mut e, now);
        assert_eq!(
            out,
            [Out::All(
                "Economy yesterday: money supply 100 coins; the server made 100 coins and destroyed 0 coins. /eco stats"
                    .into()
            )]
        );
        assert!(r.tick(&mut e, now + 1000).0.is_empty(), "once");
        let json = r.web(&mut e, now);
        assert_eq!(json["schema"], 1);
        assert_eq!(json["days"][0]["minted"], 100);
        assert!(json.get("treasury").is_none());
        assert!(
            json["daily_report"]
                .as_str()
                .unwrap()
                .starts_with("Economy yesterday")
        );
        r.store(&mut e);
        e.save().unwrap();
        let dir = std::env::temp_dir().join(format!("smp-report-daily-{}", std::process::id()));
        let e2 = Economy::open(&dir, EcoConfig::default()).unwrap();
        let r2 = Report::open(&e2, Config::default());
        assert_eq!(r2.state.days.len(), 1);
        assert_eq!(r2.state.day, (T0 + DAY_MS).div_euclid(DAY_MS));
    }
}
