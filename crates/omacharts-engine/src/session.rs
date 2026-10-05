//! Regular and extended trading hours.
//!
//! A free feed hands back every bar it has, including the thin overnight ones.
//! Those matter sometimes and ruin a chart the rest of the time: a handful of
//! trades at 3am stretch the price scale and leave the session everyone
//! actually traded squashed into a corner.
//!
//! So the chart can ask for regular hours only. Each market keeps its own
//! clock: New York's cash session is what "RTH" means for most of what this
//! app charts — US equities, the index futures that track them, and the US
//! indexes themselves — and Taipei's is the TWSE's and the TPEx's.
//! Instruments that keep hours this module does not know, or no hours at all,
//! are left alone, because filtering them would only throw data away.

use chrono::{Datelike, TimeZone, Timelike, Weekday};
use chrono_tz::America::New_York;
use chrono_tz::Asia::Taipei;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::bars::Bar;
use crate::symbols::{Instrument, InstrumentKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Session {
    /// Everything the provider sends, overnight included.
    #[default]
    Extended,
    /// The cash session only.
    Regular,
}

impl Session {
    pub const ALL: [Session; 2] = [Session::Extended, Session::Regular];

    pub fn label(self) -> &'static str {
        match self {
            Session::Extended => "Extended hours",
            Session::Regular => "Regular hours only",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Session::Extended => "extended",
            Session::Regular => "regular",
        }
    }

    pub fn from_key(key: &str) -> Option<Session> {
        Session::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// One cash market's day, in its own wall-clock time.
///
/// Two windows rather than one, and deliberately, because they answer
/// different questions. `regular` says which bars belong on a regular-hours
/// chart; `trading` says whether another bar is still coming at all — and
/// somebody with a chart open at eight in the morning is watching it
/// precisely because the pre-market is moving.
struct Market {
    zone: Tz,
    /// Minutes past local midnight, open inclusive and close exclusive.
    regular: (u32, u32),
    trading: (u32, u32),
}

impl Market {
    /// Minutes past midnight and the weekday, where this market keeps them.
    ///
    /// Timezone-aware rather than a fixed offset, because a session keeps its
    /// local hours across daylight saving while its UTC offset moves.
    fn local(&self, ts: i64) -> Option<(Weekday, u32)> {
        let local = self.zone.timestamp_opt(ts, 0).single()?;
        Some((local.weekday(), local.hour() * 60 + local.minute()))
    }

    /// Is `ts` a weekday inside one of this market's windows?
    fn open(&self, (start, end): (u32, u32), ts: i64) -> bool {
        self.local(ts).is_some_and(|(day, minutes)| weekday(day) && (start..end).contains(&minutes))
    }
}

fn weekday(day: Weekday) -> bool {
    !matches!(day, Weekday::Sat | Weekday::Sun)
}

/// The US cash market. The bar stamped at 16:00 belongs to the post-market,
/// not to this session; the pre- and post-market run 04:00 to 20:00.
const NEW_YORK: Market = Market {
    zone: New_York,
    regular: (9 * 60 + 30, 16 * 60),
    trading: (4 * 60, 20 * 60),
};

/// The Taiwan Stock Exchange and the Taipei Exchange, which keep one clock.
///
/// Continuous trading runs 09:00 to 13:25 and the closing auction prints at
/// 13:30 — a bar stamped 13:30 *is* the close, unlike New York's 16:00 bar,
/// so the regular window runs a minute past it to keep it. The trading window
/// is wider at both ends: orders are taken from 08:30, and the fixed-price
/// after-hours session runs 14:00 to 14:30. Taiwan has no daylight saving,
/// so this is 01:00 to 05:30 UTC all year.
const TAIPEI: Market = Market {
    zone: Taipei,
    regular: (9 * 60, 13 * 60 + 31),
    trading: (8 * 60 + 30, 14 * 60 + 30),
};

/// Minutes past midnight, New York time, of the hour the futures session
/// breaks for each day, which is also where its week starts and ends.
const BREAK_START: u32 = 17 * 60;
const BREAK_END: u32 = 18 * 60;

/// The cash market whose clock this instrument keeps, when it is one this
/// module knows.
///
/// FX and crypto have no cash session. A listing says where it trades with
/// its suffix — `.TW` for the TWSE, `.TWO` for the TPEx, and any other is
/// abroad on hours this module does not know. An index never carries one, so
/// it says so with its currency instead: the DAX's session is 09:00-17:30 in
/// Frankfurt and the Nikkei's has closed before New York opens. Holding those
/// to 09:30-16:00 in New York keeps two hours of the DAX's day and none at
/// all of the Nikkei's.
fn market(instrument: &Instrument) -> Option<&'static Market> {
    if !matches!(
        instrument.kind,
        InstrumentKind::Equity | InstrumentKind::Etf | InstrumentKind::Index | InstrumentKind::FutureRoot
    ) {
        return None;
    }
    match (instrument.suffix.as_deref(), instrument.currency.as_deref()) {
        (Some("TW" | "TWO"), _) | (None, Some("TWD")) => Some(&TAIPEI),
        // Unknown currency is treated as dollars: it is what a ticker typed
        // into the search field with nothing behind it turns out to be.
        (None, None | Some("USD")) => Some(&NEW_YORK),
        _ => None,
    }
}

/// Does restricting to regular hours mean anything for this instrument?
pub fn has_regular_hours(instrument: &Instrument) -> bool {
    market(instrument).is_some()
}

/// Keep only the bars inside the cash session.
///
/// A no-op for daily and coarser bars — one bar already is a session — and for
/// instruments with no cash session to speak of.
pub fn filter(bars: &[Bar], session: Session, instrument: &Instrument, intraday: bool) -> Vec<Bar> {
    if session == Session::Extended || !intraday {
        return bars.to_vec();
    }
    let Some(market) = market(instrument) else {
        return bars.to_vec();
    };
    bars.iter().copied().filter(|bar| market.open(market.regular, bar.ts)).collect()
}

/// Is this instant inside the New York cash session on a weekday?
pub fn in_regular_hours(ts: i64) -> bool {
    NEW_YORK.open(NEW_YORK.regular, ts)
}

/// Could another bar still arrive for this instrument at `ts`?
///
/// The question a chart left open has to answer before fetching itself again.
/// Asking a provider for bars that cannot exist spends a rate limit on a
/// guaranteed empty reply, and a watchlist of forty symbols asking it all
/// weekend is how an address gets throttled for the Monday open.
///
/// Generous at every edge on purpose. Being wrong in this direction costs one
/// request that comes back with nothing; being wrong in the other leaves a
/// chart quietly stale while the thing it charts is moving, which is the only
/// failure here anybody would notice. There is no holiday calendar, so a
/// public holiday reads as an ordinary weekday and costs a handful of empty
/// replies — the same trade, taken knowingly.
pub fn is_trading(instrument: &Instrument, ts: i64) -> bool {
    // Two very different groups come out the same way here. FX and crypto
    // never close, so another bar is always coming. A foreign listing, or an
    // index priced somewhere else, keeps hours this module knows nothing
    // about — and guessing would mean refusing to refresh a Madrid listing
    // right through Madrid's own session, which is worse than not asking.
    let Some(market) = market(instrument) else {
        return true;
    };
    if instrument.kind == InstrumentKind::FutureRoot {
        return NEW_YORK.local(ts).is_some_and(|(day, minutes)| futures_are_trading(day, minutes));
    }
    market.open(market.trading, ts)
}

/// The futures week: Sunday evening through to Friday afternoon, broken for an
/// hour at the end of each session.
///
/// Worth answering separately because this is the one thing on a chart that
/// trades while everybody is asleep, and the small hours are exactly when
/// somebody has an index future open. Holding it to the cash market's day
/// would switch refreshing off for the half of its week that people watch it
/// for.
fn futures_are_trading(weekday: Weekday, minutes: u32) -> bool {
    match weekday {
        Weekday::Sat => false,
        Weekday::Sun => minutes >= BREAK_END,
        Weekday::Fri => minutes < BREAK_START,
        _ => !(BREAK_START..BREAK_END).contains(&minutes),
    }
}

#[cfg(test)]
mod tests {
    use chrono_tz::Asia::Tokyo;

    use super::*;

    fn instrument(kind: InstrumentKind, suffix: Option<&str>) -> Instrument {
        Instrument {
            symbol: "X".into(),
            name: "X".into(),
            kind,
            suffix: suffix.map(str::to_string),
            currency: None,
            tier: 0,
            session_origin: 0,
            overrides: Vec::new(),
            exchange: None,
            popularity: 0,
            local_name: None,
        }
    }

    /// The same instrument, quoted somewhere in particular.
    fn priced_in(kind: InstrumentKind, currency: &str) -> Instrument {
        Instrument { currency: Some(currency.into()), ..instrument(kind, None) }
    }

    fn bar(ts: i64) -> Bar {
        Bar { ts, open: 1.0, high: 1.0, low: 1.0, close: 1.0, volume: 1.0 }
    }

    /// 2024-03-13 was a Wednesday, in US daylight time (UTC-4).
    fn wednesday_at(hour: u32, minute: u32) -> i64 {
        New_York
            .with_ymd_and_hms(2024, 3, 13, hour, minute, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    /// 2024-01-10, a Wednesday in standard time (UTC-5).
    fn winter_wednesday_at(hour: u32, minute: u32) -> i64 {
        New_York
            .with_ymd_and_hms(2024, 1, 10, hour, minute, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn the_cash_session_runs_from_the_open_to_the_close() {
        assert!(!in_regular_hours(wednesday_at(9, 29)), "before the bell");
        assert!(in_regular_hours(wednesday_at(9, 30)), "the open");
        assert!(in_regular_hours(wednesday_at(12, 0)));
        assert!(in_regular_hours(wednesday_at(15, 59)));
        assert!(!in_regular_hours(wednesday_at(16, 0)), "the close belongs to the next session");
        assert!(!in_regular_hours(wednesday_at(3, 0)), "overnight");
    }

    #[test]
    fn the_session_keeps_its_local_hours_across_daylight_saving() {
        // Same wall-clock times, different UTC offsets.
        assert!(in_regular_hours(winter_wednesday_at(9, 30)));
        assert!(!in_regular_hours(winter_wednesday_at(9, 29)));
        assert!(in_regular_hours(winter_wednesday_at(15, 59)));
        // And the two dates really are on different offsets.
        assert_ne!(
            wednesday_at(9, 30) % 86_400,
            winter_wednesday_at(9, 30) % 86_400,
            "the fixture dates should straddle the change"
        );
    }

    #[test]
    fn weekends_are_not_the_cash_session() {
        let saturday = New_York.with_ymd_and_hms(2024, 3, 16, 12, 0, 0).single().unwrap();
        assert!(!in_regular_hours(saturday.timestamp()));
    }

    #[test]
    fn filtering_keeps_only_the_session() {
        let bars = vec![
            bar(wednesday_at(4, 0)),
            bar(wednesday_at(9, 30)),
            bar(wednesday_at(12, 0)),
            bar(wednesday_at(18, 0)),
        ];
        let stock = instrument(InstrumentKind::Equity, None);
        let kept = filter(&bars, Session::Regular, &stock, true);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].ts, wednesday_at(9, 30));
    }

    #[test]
    fn extended_hours_keeps_everything() {
        let bars: Vec<Bar> = (0..24).map(|h| bar(wednesday_at(h, 0))).collect();
        let stock = instrument(InstrumentKind::Equity, None);
        assert_eq!(filter(&bars, Session::Extended, &stock, true).len(), bars.len());
    }

    #[test]
    fn daily_bars_are_never_filtered() {
        // One daily bar already is a session; dropping it for being stamped
        // outside 09:30 would empty the chart.
        let bars = vec![bar(wednesday_at(0, 0)), bar(wednesday_at(0, 0) + 86_400)];
        let stock = instrument(InstrumentKind::Equity, None);
        assert_eq!(filter(&bars, Session::Regular, &stock, false).len(), 2);
    }

    #[test]
    fn instruments_without_a_cash_session_are_left_alone() {
        let bars: Vec<Bar> = (0..24).map(|h| bar(wednesday_at(h, 0))).collect();
        for kind in [InstrumentKind::Fx, InstrumentKind::Crypto] {
            let it = instrument(kind, None);
            assert!(!has_regular_hours(&it));
            assert_eq!(filter(&bars, Session::Regular, &it, true).len(), bars.len());
        }
        // Nor does a foreign listing, whose hours are not New York's.
        let madrid = instrument(InstrumentKind::Equity, Some("MC"));
        assert!(!has_regular_hours(&madrid));
        assert_eq!(filter(&bars, Session::Regular, &madrid, true).len(), bars.len());
    }

    #[test]
    fn an_index_quoted_abroad_keeps_its_own_hours() {
        // The Nikkei's whole session is overnight in New York, so holding it
        // to the cash session there would leave nothing on the chart at all.
        let nikkei = priced_in(InstrumentKind::Index, "JPY");
        assert!(!has_regular_hours(&nikkei));

        let bars: Vec<Bar> = (9..15)
            .map(|h| bar(Tokyo.with_ymd_and_hms(2024, 3, 13, h, 0, 0).single().unwrap().timestamp()))
            .collect();
        assert!(!bars.iter().any(|b| in_regular_hours(b.ts)), "the fixture should be overnight");
        assert_eq!(filter(&bars, Session::Regular, &nikkei, true).len(), bars.len());

        // The DAX fares differently and no better: Frankfurt's 09:00-17:30 and
        // New York's 09:30-16:00 overlap for about two hours, so filtering
        // keeps a quarter of the day and calls it the session.
        let dax = priced_in(InstrumentKind::Index, "EUR");
        assert!(!has_regular_hours(&dax));
    }

    #[test]
    fn a_dollar_index_still_has_a_cash_session() {
        // The S&P really is on New York hours, and so is a ticker typed into
        // the search field with no listing behind it to say otherwise.
        assert!(has_regular_hours(&priced_in(InstrumentKind::Index, "USD")));
        assert!(has_regular_hours(&instrument(InstrumentKind::Equity, None)));

        let bars = vec![bar(wednesday_at(4, 0)), bar(wednesday_at(12, 0))];
        let spx = priced_in(InstrumentKind::Index, "USD");
        assert_eq!(filter(&bars, Session::Regular, &spx, true).len(), 1);
    }

    /// A moment in New York, for the days of the week the futures session
    /// cares about. March 2024 runs Friday the 15th, Saturday the 16th,
    /// Sunday the 17th, Monday the 18th.
    fn ny(day: u32, hour: u32, minute: u32) -> i64 {
        New_York
            .with_ymd_and_hms(2024, 3, day, hour, minute, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    /// The window a chart refetches inside is the extended one, not the cash
    /// session: somebody with a chart open before the bell is watching the
    /// pre-market, and telling them nothing is coming would be wrong.
    #[test]
    fn a_share_is_trading_through_the_pre_and_post_market() {
        let stock = instrument(InstrumentKind::Equity, None);
        assert!(!is_trading(&stock, ny(13, 3, 59)), "before the pre-market");
        assert!(is_trading(&stock, ny(13, 4, 0)), "the pre-market opens");
        assert!(is_trading(&stock, ny(13, 9, 0)), "an hour before the bell");
        assert!(is_trading(&stock, ny(13, 12, 0)), "the cash session");
        assert!(is_trading(&stock, ny(13, 18, 0)), "after the bell, still printing");
        assert!(!is_trading(&stock, ny(13, 20, 0)), "the post-market closes");
        assert!(!is_trading(&stock, ny(13, 2, 0)), "the middle of the night");
    }

    /// The case that saves the most requests by far: a chart left open over a
    /// weekend has two days in which nothing it could ask for exists.
    #[test]
    fn nothing_listed_is_trading_at_the_weekend() {
        let stock = instrument(InstrumentKind::Equity, None);
        assert!(!is_trading(&stock, ny(16, 12, 0)), "Saturday");
        assert!(!is_trading(&stock, ny(17, 12, 0)), "Sunday");
    }

    /// Futures keep their own week, and it is most of one. Holding them to the
    /// cash market's hours would switch refreshing off exactly when somebody
    /// watches an index future: overnight.
    #[test]
    fn a_future_trades_overnight_and_almost_all_week() {
        let future = instrument(InstrumentKind::FutureRoot, None);
        assert!(is_trading(&future, ny(13, 3, 0)), "three in the morning, and printing");
        assert!(is_trading(&future, ny(13, 12, 0)), "the middle of the day");
        assert!(is_trading(&future, ny(13, 23, 0)), "and late at night");

        // The hour each session breaks for.
        assert!(!is_trading(&future, ny(13, 17, 30)), "the daily break");
        assert!(is_trading(&future, ny(13, 18, 0)), "the next session opens");

        // The two ends of the week.
        assert!(is_trading(&future, ny(15, 16, 0)), "Friday afternoon");
        assert!(!is_trading(&future, ny(15, 17, 30)), "Friday's close is the week's");
        assert!(!is_trading(&future, ny(16, 12, 0)), "Saturday, like everything else");
        assert!(!is_trading(&future, ny(17, 12, 0)), "Sunday lunchtime, still shut");
        assert!(is_trading(&future, ny(17, 18, 0)), "Sunday evening, and the week restarts");
    }

    /// A market with no close has no moment at which asking is pointless.
    #[test]
    fn what_never_closes_is_always_trading() {
        for kind in [InstrumentKind::Crypto, InstrumentKind::Fx] {
            let it = instrument(kind, None);
            assert!(is_trading(&it, ny(17, 3, 0)), "{kind:?} at 3am on a Sunday");
            assert!(is_trading(&it, ny(13, 12, 0)), "{kind:?} midweek");
        }
    }

    /// Hours this module does not know are hours it must not rule on. A
    /// Madrid listing's session is the middle of New York's night, so reading
    /// "closed" off New York's clock would leave it stale all day.
    #[test]
    fn a_listing_whose_hours_we_do_not_know_is_treated_as_trading() {
        let madrid = instrument(InstrumentKind::Equity, Some("MC"));
        assert!(is_trading(&madrid, ny(13, 4, 0)), "ten in the morning in Madrid");
        assert!(is_trading(&madrid, ny(13, 22, 0)), "and at a time we cannot rule out");

        let nikkei = priced_in(InstrumentKind::Index, "JPY");
        assert!(is_trading(&nikkei, ny(13, 2, 0)));
    }

    /// The cash session and the window a chart refetches in are not the same
    /// window, and nothing should quietly collapse them into one.
    #[test]
    fn trading_is_a_wider_window_than_the_cash_session() {
        let stock = instrument(InstrumentKind::Equity, None);
        let before_the_bell = ny(13, 8, 0);
        assert!(!in_regular_hours(before_the_bell), "not a regular-hours bar");
        assert!(is_trading(&stock, before_the_bell), "but another one is coming");
    }

    /// A moment in Taipei. October 2026 runs Friday the 2nd, Saturday the
    /// 3rd, Sunday the 4th, Monday the 5th.
    fn taipei(day: u32, hour: u32, minute: u32) -> i64 {
        Taipei.with_ymd_and_hms(2026, 10, day, hour, minute, 0).single().unwrap().timestamp()
    }

    #[test]
    fn a_taiwan_listing_keeps_taipei_hours() {
        for suffix in ["TW", "TWO"] {
            let tsmc = Instrument {
                currency: Some("TWD".into()),
                ..instrument(InstrumentKind::Equity, Some(suffix))
            };
            assert!(has_regular_hours(&tsmc), ".{suffix} has a cash session");
            let bars: Vec<Bar> = [(8, 59), (9, 0), (11, 0), (13, 25), (13, 30), (13, 31), (14, 0)]
                .iter()
                .map(|(h, m)| bar(taipei(5, *h, *m)))
                .collect();
            let kept: Vec<i64> =
                filter(&bars, Session::Regular, &tsmc, true).iter().map(|b| b.ts).collect();
            assert_eq!(
                kept,
                vec![taipei(5, 9, 0), taipei(5, 11, 0), taipei(5, 13, 25), taipei(5, 13, 30)],
                "09:00 through the 13:30 closing auction, and nothing either side"
            );
        }
    }

    /// The Taipei session is the middle of New York's night. Reading it off
    /// New York's clock, the way every suffixed listing used to be treated,
    /// would either call it shut all day or never call it shut at all.
    #[test]
    fn a_taiwan_listing_is_trading_only_through_its_own_day() {
        let tsmc = instrument(InstrumentKind::Equity, Some("TW"));
        assert!(!is_trading(&tsmc, taipei(5, 8, 29)), "before orders are taken");
        assert!(is_trading(&tsmc, taipei(5, 8, 30)), "the pre-open");
        assert!(is_trading(&tsmc, taipei(5, 10, 0)), "the session");
        assert!(is_trading(&tsmc, taipei(5, 14, 15)), "the after-hours fixed-price session");
        assert!(!is_trading(&tsmc, taipei(5, 14, 30)), "everything has closed");
        assert!(!is_trading(&tsmc, taipei(5, 22, 30)), "the US open is the Taipei night");
        assert!(!is_trading(&tsmc, taipei(3, 10, 0)), "Saturday");
        assert!(!is_trading(&tsmc, taipei(4, 10, 0)), "Sunday");
        assert!(is_trading(&tsmc, taipei(2, 10, 0)), "Friday");
    }

    #[test]
    fn the_taiex_keeps_taipei_hours_too() {
        // An index never carries a suffix, so its currency is what says where
        // it is priced.
        let taiex = priced_in(InstrumentKind::Index, "TWD");
        assert!(has_regular_hours(&taiex));
        assert!(is_trading(&taiex, taipei(5, 10, 0)));
        assert!(!is_trading(&taiex, taipei(5, 22, 0)));
        let tokyo = instrument(InstrumentKind::Equity, Some("T"));
        assert!(is_trading(&tokyo, taipei(5, 22, 0)), "Tokyo is not Taipei, and is not ruled on");
    }

    #[test]
    fn keys_round_trip() {
        for session in Session::ALL {
            assert_eq!(Session::from_key(session.key()), Some(session));
        }
        assert_eq!(Session::from_key("nonsense"), None);
    }
}
