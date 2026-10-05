//! The instrument inventory and the search over it.
//!
//! Search never touches a database. The whole inventory is loaded once into a
//! flat vector with lowercased keys and first-character buckets; a query scans
//! a few hundred candidates, not the whole universe. The budget is under a
//! millisecond so that search-as-you-type is always safe.
//!
//! Ranking matters more than the index. Typing `GC` must surface gold futures,
//! not some microcap that happens to share the letters — which is what `tier`
//! is for.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    Index,
    FutureRoot,
    Equity,
    Etf,
    Fx,
    Crypto,
}

impl InstrumentKind {
    pub fn label(self) -> &'static str {
        match self {
            InstrumentKind::Index => "Index",
            InstrumentKind::FutureRoot => "Futures",
            InstrumentKind::Equity => "Stock",
            InstrumentKind::Etf => "ETF",
            InstrumentKind::Fx => "FX",
            InstrumentKind::Crypto => "Crypto",
        }
    }

    /// Does this trade around the clock?
    ///
    /// A market that never closes has no auction open — today's open is
    /// yesterday's close — which matters anywhere a bar's open is treated as
    /// a separate price rather than a continuation.
    pub fn is_continuous(self) -> bool {
        matches!(self, InstrumentKind::Fx | InstrumentKind::Crypto)
    }

    pub fn from_key(key: &str) -> Option<InstrumentKind> {
        Some(match key {
            "index" => InstrumentKind::Index,
            "future_root" => InstrumentKind::FutureRoot,
            "equity" => InstrumentKind::Equity,
            "etf" => InstrumentKind::Etf,
            "fx" => InstrumentKind::Fx,
            "crypto" => InstrumentKind::Crypto,
            _ => return None,
        })
    }
}

/// One tradeable thing, in nobody's symbology in particular.
///
/// `symbol` is canonical and neutral: `GSPC`, not `^GSPC`; `ES`, not `ES=F`.
/// Turning it into a provider's spelling is the provider's job.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Instrument {
    pub symbol: String,
    pub name: String,
    pub kind: InstrumentKind,
    /// Yahoo-style exchange suffix for non-US listings: `L`, `DE`, `MC`…
    pub suffix: Option<String>,
    pub currency: Option<String>,
    /// 0 is a curated major, 3 is excluded from search.
    pub tier: u8,
    /// Seconds past midnight UTC that this instrument's session opens. Drives
    /// session-aware resampling; 0 for anything that trades a normal day.
    pub session_origin: i64,
    /// Per-provider spellings that no template produces.
    pub overrides: Vec<(String, String)>,
    /// Where it trades, when the source knew. Unset for anything whose venue
    /// can be worked out from its suffix or its kind.
    pub exchange: Option<String>,
    /// How well known this is among things of its own sort, 1 (obscure) to 9
    /// (household name); 0 when nothing ranked it. Curated rows leave it at 0
    /// and win on `tier` instead.
    pub popularity: u8,
}

impl Instrument {
    pub fn override_for(&self, adapter: &str) -> Option<&str> {
        self.overrides
            .iter()
            .find(|(a, _)| a == adapter)
            .map(|(_, s)| s.as_str())
    }

    /// What the UI shows as the instrument's ticker.
    /// Where it trades, in the form people say out loud.
    ///
    /// Taken from the listing when the source gave one, and otherwise worked
    /// out: a Yahoo suffix names a venue, and everything with no suffix and no
    /// listing behind it is one of the handful of kinds that has an obvious
    /// home.
    pub fn exchange_label(&self) -> Option<&str> {
        if let Some(named) = self.exchange.as_deref() {
            return Some(named);
        }
        if let Some(suffix) = self.suffix.as_deref() {
            return Some(match suffix {
                "MC" => "BME",
                "L" => "LSE",
                "DE" => "XETRA",
                "PA" => "Euronext",
                "SW" => "SIX",
                "T" => "TSE",
                "TO" => "TSX",
                "AS" => "Euronext",
                "MI" => "Borsa Italiana",
                "HK" => "HKEX",
                other => other,
            });
        }
        match self.kind {
            InstrumentKind::Fx => Some("FX"),
            InstrumentKind::Crypto => Some("Crypto"),
            _ => None,
        }
    }

    pub fn display_symbol(&self) -> String {
        match &self.suffix {
            Some(s) => format!("{}.{}", self.symbol, s),
            None => self.symbol.clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SearchHit {
    pub index: usize,
    pub score: i32,
}

/// Lowercased keys and first-character buckets over an instrument list.
pub struct SearchIndex {
    items: Vec<Instrument>,
    symbol_lc: Vec<String>,
    name_lc: Vec<String>,
    /// First character of the symbol, and of every word of the name, to the
    /// items it could match.
    buckets: HashMap<char, Vec<u32>>,
}

impl SearchIndex {
    pub fn new(items: Vec<Instrument>) -> SearchIndex {
        let mut symbol_lc = Vec::with_capacity(items.len());
        let mut name_lc = Vec::with_capacity(items.len());
        let mut buckets: HashMap<char, Vec<u32>> = HashMap::new();

        for (i, item) in items.iter().enumerate() {
            let sym = item.symbol.to_lowercase();
            let name = item.name.to_lowercase();

            let mut firsts: Vec<char> = Vec::new();
            if let Some(c) = sym.chars().next() {
                firsts.push(c);
            }
            for word in name.split(|c: char| !c.is_alphanumeric()) {
                if let Some(c) = word.chars().next() {
                    firsts.push(c);
                }
            }
            firsts.sort_unstable();
            firsts.dedup();
            for c in firsts {
                buckets.entry(c).or_default().push(i as u32);
            }

            symbol_lc.push(sym);
            name_lc.push(name);
        }

        SearchIndex { items, symbol_lc, name_lc, buckets }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Instrument> {
        self.items.get(index)
    }

    pub fn items(&self) -> &[Instrument] {
        &self.items
    }

    /// Exact lookup by canonical symbol and suffix.
    ///
    /// The watchlist stores what the user picked, not an index position, so it
    /// survives the inventory growing or being regenerated.
    pub fn find(&self, symbol: &str, suffix: Option<&str>) -> Option<&Instrument> {
        self.items
            .iter()
            .find(|i| i.symbol == symbol && i.suffix.as_deref() == suffix)
    }

    /// One ticker as somebody wrote it: `AAPL`, `SAP.DE`, `BRK.B`.
    ///
    /// Taken whole first, so a dot that is part of the ticker stays part of
    /// it, and only then as a ticker and a venue suffix.
    pub fn lookup(&self, ticker: &str) -> Option<&Instrument> {
        self.find(ticker, None).or_else(|| {
            let (symbol, suffix) = ticker.rsplit_once('.')?;
            self.find(symbol, Some(suffix))
        })
    }

    /// Every instrument a pasted list names, in the order it names them, and
    /// the tickers it names that are not instruments.
    ///
    /// `venue` is the suffix for a ticker written without one, as
    /// `watchlist add --suffix` gives it. See [`tickers`] for the text.
    pub fn resolve_list(&self, text: &str, venue: Option<&str>) -> (Vec<&Instrument>, Vec<String>) {
        let mut found: Vec<&Instrument> = Vec::new();
        let mut unknown = Vec::new();
        for ticker in tickers(text) {
            let instrument = match venue {
                Some(_) => self.find(&ticker, venue),
                None => self.lookup(&ticker),
            };
            match instrument {
                // Two spellings of one listing — SAN.MC and SAN --suffix MC —
                // are still one row.
                Some(instrument) if !found.contains(&instrument) => found.push(instrument),
                Some(_) => {}
                None => unknown.push(ticker),
            }
        }
        (found, unknown)
    }

    /// The curated majors, for an empty search field.
    pub fn featured(&self, limit: usize) -> Vec<SearchHit> {
        let mut hits: Vec<SearchHit> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.tier == 0)
            .map(|(index, _)| SearchHit { index, score: 0 })
            .collect();
        hits.truncate(limit);
        hits
    }

    /// Best matches for `query`, best first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.featured(limit);
        }
        let Some(first) = q.chars().next() else {
            return Vec::new();
        };
        let Some(candidates) = self.buckets.get(&first) else {
            return Vec::new();
        };

        let mut hits: Vec<SearchHit> = Vec::new();
        for &i in candidates {
            let i = i as usize;
            if self.items[i].tier >= 3 {
                continue;
            }
            if let Some(score) = self.score(i, &q) {
                hits.push(SearchHit { index: i, score });
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| self.symbol_lc[a.index].len().cmp(&self.symbol_lc[b.index].len()))
                .then_with(|| self.symbol_lc[a.index].cmp(&self.symbol_lc[b.index]))
        });
        hits.truncate(limit);
        hits
    }

    /// `None` when the query does not match at all.
    ///
    /// The tiers are what make this feel right: an exact ticker always wins,
    /// but between two equally good textual matches the one people actually
    /// meant — an index, a front-month future, a mega-cap — comes first.
    ///
    /// A name that *is* the query counts as exact too. People type `DAX`
    /// meaning the index, whose canonical symbol is `GDAXI`; without this the
    /// NASDAQ ETF that happens to own the ticker would win on spelling alone.
    fn score(&self, i: usize, q: &str) -> Option<i32> {
        let symbol = &self.symbol_lc[i];
        let name = &self.name_lc[i];
        let item = &self.items[i];

        // Two ladders rather than one, because a ticker and a name are not
        // the same kind of evidence and the weights below treat them
        // differently. The rungs are the same numbers either way, so taking
        // the better of the two is the chain of `else if` this used to be.
        let symbolic = if symbol == q {
            Some(EXACT)
        } else if symbol.starts_with(q) {
            // A longer ticker is a worse answer: `aa` means Alcoa before it
            // means AAON.
            Some(700 - (symbol.len() - q.len()).min(20) as i32 * 4)
        } else if symbol.contains(q) {
            Some(320)
        } else {
            None
        };

        let nominal = if name == q {
            Some(EXACT)
        } else if name.split(|c: char| !c.is_alphanumeric()).any(|w| w == q) {
            Some(500)
        } else if name.starts_with(q) {
            Some(450)
        } else if name
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w.starts_with(q))
        {
            Some(300)
        } else if name.contains(q) {
            Some(180)
        } else {
            None
        };

        let tier_weight = match item.tier {
            0 => 120,
            1 => 60,
            _ => 0,
        };
        // Indexes and futures are what this app is for; nudge them up when the
        // textual match is otherwise a tie.
        let kind_weight = match item.kind {
            InstrumentKind::Index | InstrumentKind::FutureRoot => 30,
            InstrumentKind::Etf => 12,
            _ => 0,
        };
        // Six a band, so fame tops out at 54, and the ceiling is what matters:
        // it sits just under the 60 a tier-1 row carries, so the most famous
        // listing a feed can name still loses to the least of the instruments
        // someone chose to curate — searching `bank` finds Bank of America,
        // not whichever bank ETF traded most yesterday. Well clear of the
        // 4-a-character penalty a longer ticker pays, though, which is the
        // whole point: `aa` should find Alcoa before AAON. It reorders a band
        // from within; it never promotes one match over a better one.
        let fame = item.popularity as i32 * 6;
        let prior = tier_weight + kind_weight + fame;

        // A ticker is a label anyone can collide with: eleven thousand of them
        // are four letters or fewer, so `tes` being the start of one is weak
        // evidence and gets the prior once. A name is something you have to
        // know before you can type it, so matching one is evidence twice over
        // — that this is the row you meant, and that you knew its name at all
        // — and the prior counts twice. Without this a test issue called TEST
        // outranked Tesla, and four leveraged ETFs outranked crude oil.
        //
        // It cannot promote anything over an exact match. The best a partial
        // can reach is 500 + 2 × 204 = 908, against the 1000 an exact scores
        // before a single weight is added, and
        // `an_exact_ticker_outscores_the_best_possible_partial` holds that
        // moat open.
        let by_symbol = symbolic.map(|textual| textual + prior);
        let by_name = nominal.map(|textual| textual + prior * 2);
        by_symbol.max(by_name)
    }
}

/// The tickers in a list somebody pasted or exported, upper-cased, each once.
///
/// Separated by commas, semicolons or any whitespace, so one per line, a
/// spreadsheet column and `AAPL, MSFT` all read the same. A watchlist
/// exported from another charting tool works as it is: a field starting with
/// `#` is a `###Section` heading, names with spaces in them included, and an
/// exchange prefix — `NASDAQ:AAPL` — keeps only the ticker. Quotes, which a
/// CSV wraps fields in, are dropped.
pub fn tickers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for field in text.split([',', ';', '\n']) {
        if field.trim_start().starts_with('#') {
            continue;
        }
        for token in field.split_whitespace() {
            let token = token.trim_matches('"');
            let ticker = token.rsplit(':').next().unwrap_or(token).to_uppercase();
            if !ticker.is_empty() && !out.contains(&ticker) {
                out.push(ticker);
            }
        }
    }
    out
}

/// What a query that *is* the ticker, or *is* the name, scores before weights.
///
/// Far enough above every partial band that no combination of tier, kind and
/// fame can reach it from below: typing GC gives gold and typing TSLA gives
/// Tesla, whatever else happens to share the letters.
const EXACT: i32 = 1000;

/// The most any row can add to its textual band: tier 0, an index or a future,
/// and a household name. Only a bound, and only the test that pins it uses it.
#[cfg(test)]
const MAX_PRIOR: i32 = 120 + 30 + 9 * 6;

/// The curated inventory shipped in the binary.
///
/// Enough to use the app the moment it is installed, and the thing search is
/// tuned against. The generated database widens this to every US listing
/// without changing anything here.
pub fn seed() -> Vec<Instrument> {
    parse_seed(include_str!("seed.tsv"))
}

/// `kind<TAB>symbol<TAB>name<TAB>suffix<TAB>currency<TAB>tier<TAB>session_origin<TAB>yahoo_override<TAB>exchange<TAB>popularity`
///
/// Everything past `tier` is optional, so a file written before a column
/// existed still parses — which is what lets the generated half gain columns
/// without the curated half being rewritten to match.
pub fn parse_seed(text: &str) -> Vec<Instrument> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 6 {
            continue;
        }
        let Some(kind) = InstrumentKind::from_key(f[0]) else {
            continue;
        };
        let blank = |s: &str| if s.is_empty() || s == "-" { None } else { Some(s.to_string()) };
        out.push(Instrument {
            kind,
            symbol: f[1].to_string(),
            name: f[2].to_string(),
            suffix: blank(f[3]),
            currency: blank(f[4]),
            tier: f[5].parse().unwrap_or(2),
            session_origin: f.get(6).and_then(|v| v.parse().ok()).unwrap_or(0),
            overrides: match f.get(7).copied().and_then(blank) {
                Some(sym) => vec![("yahoo".to_string(), sym)],
                None => Vec::new(),
            },
            exchange: f.get(8).copied().and_then(blank),
            popularity: f.get(9).and_then(|v| v.parse().ok()).unwrap_or(0),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> SearchIndex {
        SearchIndex::new(seed())
    }

    /// A generated-half row: tier 2, no overrides, as popular as you say.
    fn listed(symbol: &str, name: &str, popularity: u8) -> Instrument {
        Instrument {
            symbol: symbol.into(),
            name: name.into(),
            kind: InstrumentKind::Equity,
            suffix: None,
            currency: Some("USD".into()),
            tier: 2,
            session_origin: 0,
            overrides: Vec::new(),
            exchange: Some("NASDAQ".into()),
            popularity,
        }
    }

    /// The same, as a fund — which carries a kind weight a share does not, so
    /// it is the harder thing to outrank.
    fn fund(row: Instrument) -> Instrument {
        Instrument { kind: InstrumentKind::Etf, ..row }
    }

    #[test]
    fn the_seed_parses() {
        let items = seed();
        assert!(items.len() > 150, "seed looks short: {}", items.len());
        assert!(items.iter().any(|i| i.symbol == "GSPC"));
        assert!(items.iter().any(|i| i.symbol == "ES"));
    }

    #[test]
    fn gc_finds_gold_futures_first() {
        let idx = index();
        let hits = idx.search("gc", 5);
        let first = idx.get(hits[0].index).unwrap();
        assert_eq!(first.symbol, "GC", "got {first:?}");
        assert_eq!(first.kind, InstrumentKind::FutureRoot);
    }

    #[test]
    fn an_exact_ticker_wins() {
        let idx = index();
        for ticker in ["aapl", "es", "spy", "vix"] {
            let hits = idx.search(ticker, 5);
            assert!(!hits.is_empty(), "no hits for {ticker}");
            assert_eq!(
                idx.get(hits[0].index).unwrap().symbol.to_lowercase(),
                ticker,
                "wrong winner for {ticker}"
            );
        }
    }

    #[test]
    fn a_famous_name_beats_a_ticker_squatter() {
        // `DAX` is the index's name, not its symbol — and a NASDAQ ETF owns
        // the literal ticker. The index is what people mean.
        let mut items = seed();
        items.push(Instrument {
            symbol: "DAX".into(),
            name: "Global X DAX Germany ETF".into(),
            kind: InstrumentKind::Etf,
            suffix: None,
            currency: Some("USD".into()),
            tier: 2,
            session_origin: 0,
            overrides: Vec::new(),
            exchange: Some("NASDAQ".into()),
            popularity: 0,
        });
        let idx = SearchIndex::new(items);
        assert_eq!(idx.get(idx.search("dax", 5)[0].index).unwrap().symbol, "GDAXI");
    }

    /// A listing whose ticker happens to start with your letters, against the
    /// company whose *name* does. Nobody typing `tes` wants a structured
    /// product called TEST; they want Tesla, and the only thing separating the
    /// two is that one of them is a household name.
    #[test]
    fn a_famous_name_beats_a_ticker_that_merely_shares_the_letters() {
        let mut items = seed();
        // The curated file carries no ranking; a curated row picks one up from
        // the listing it displaces when the two halves are merged, which is
        // the state search actually runs against.
        for row in &mut items {
            if row.symbol == "TSLA" {
                row.popularity = 9;
            }
        }
        items.push(fund(listed("TEST", "YieldMax TSLA Performance & Distribution ETF", 3)));
        items.push(fund(listed("TESL", "Simplify Volt TSLA Revolution ETF", 1)));
        let idx = SearchIndex::new(items);
        assert_eq!(idx.get(idx.search("tes", 5)[0].index).unwrap().symbol, "TSLA");
    }

    /// The same thing one rung further down: `oil` is a whole word of the
    /// futures root's name, and four leveraged ETFs own tickers beginning
    /// OIL. A curated front-month contract is what the app is for.
    #[test]
    fn a_curated_name_beats_the_tickers_that_squat_on_the_word() {
        let mut items = seed();
        for (symbol, name) in [
            ("OILK", "ProShares K-1 Free Crude Oil ETF"),
            ("OILU", "MicroSectors Oil & Gas Exp. & Prod. 3x ETN"),
            ("OILD", "MicroSectors Oil & Gas Exp. & Prod. -3x ETN"),
            ("OILT", "Texas Capital Texas Oil Index ETF"),
        ] {
            items.push(fund(listed(symbol, name, 4)));
        }
        let idx = SearchIndex::new(items);
        assert_eq!(idx.get(idx.search("oil", 5)[0].index).unwrap().symbol, "CL");
    }

    /// The floor the whole search stands on: typing a ticker in full gives you
    /// that ticker. Promoting name matches is only safe while no stack of
    /// weights can climb from a partial band to the exact one, so this builds
    /// the worst case on purpose — the dullest possible exact match against a
    /// rival that is curated, an index, a household name, and matches both as
    /// a ticker prefix and as a whole word of its name.
    #[test]
    fn an_exact_ticker_outscores_the_best_possible_partial() {
        let exact = Instrument {
            symbol: "ZZZ".into(),
            name: "Nothing In Particular".into(),
            kind: InstrumentKind::Equity,
            suffix: None,
            currency: None,
            tier: 2,
            session_origin: 0,
            overrides: Vec::new(),
            exchange: None,
            popularity: 0,
        };
        let rival = Instrument {
            symbol: "ZZZA".into(),
            name: "Zzz Everything Index".into(),
            kind: InstrumentKind::Index,
            tier: 0,
            popularity: 9,
            ..exact.clone()
        };
        let idx = SearchIndex::new(vec![rival, exact]);
        let hits = idx.search("zzz", 2);
        assert_eq!(idx.get(hits[0].index).unwrap().symbol, "ZZZ");
        assert!(
            hits[0].score - hits[1].score >= EXACT - (500 + 2 * MAX_PRIOR),
            "the moat has narrowed: {} vs {}",
            hits[0].score,
            hits[1].score
        );
    }

    /// Cheap and exhaustive: every instrument anyone curated answers to its own
    /// ticker. One curated row shadowing another is the kind of thing a weight
    /// change causes and nobody notices until they type it.
    #[test]
    fn every_curated_ticker_finds_itself() {
        let idx = index();
        for item in seed() {
            let hits = idx.search(&item.symbol, 1);
            let first = idx.get(hits[0].index).unwrap();
            assert_eq!(first.symbol, item.symbol, "{} found {} first", item.symbol, first.symbol);
        }
    }

    #[test]
    fn fame_settles_a_tie_the_spelling_cannot() {
        // Both are prefix matches of the same length, so every other term in
        // the score is identical and the old ranking fell back to alphabetical
        // order — which put a microcap above the aluminium company.
        let idx = SearchIndex::new(vec![
            listed("AAON", "AAON Inc.", 3),
            listed("AAPX", "Some Obscure Thing", 1),
            listed("AACQ", "Alcoa-ish Corporation", 9),
        ]);
        let hits = idx.search("aa", 5);
        assert_eq!(idx.get(hits[0].index).unwrap().symbol, "AACQ");
        assert_eq!(idx.get(hits[2].index).unwrap().symbol, "AAPX");
    }

    #[test]
    fn fame_never_promotes_a_worse_match() {
        // The whole point of capping fame below the gaps between the textual
        // bands: someone typing a ticker in full gets that ticker, however
        // famous the thing that merely starts with it.
        let idx = SearchIndex::new(vec![
            listed("NVDA", "NVIDIA Corporation", 9),
            listed("NVD", "Nothing Very Dramatic", 1),
        ]);
        assert_eq!(idx.get(idx.search("nvd", 5)[0].index).unwrap().symbol, "NVD");
    }

    #[test]
    fn a_curated_row_outranks_a_famous_generated_one() {
        // 120 for a hand-picked row against 72 for the most famous listing
        // there is. Curation is a stronger signal than any feed, because it is
        // the one signal that knows what this app is for.
        let mut items = seed();
        items.push(listed("SPYY", "Something Else Entirely", 9));
        let idx = SearchIndex::new(items);
        assert_eq!(idx.get(idx.search("spy", 5)[0].index).unwrap().symbol, "SPY");
    }

    #[test]
    fn a_row_from_before_the_column_existed_still_parses() {
        // The curated half has eight columns and is not regenerated when the
        // generated half gains a ninth or a tenth. Both must keep loading.
        let eight = "equity\tOLD\tOld Eight Column\t-\tUSD\t2\t0\t-";
        let ten = "equity\tNEW\tNew Ten Column\t-\tUSD\t2\t0\t-\tNASDAQ\t7";
        let items = parse_seed(&format!("{eight}\n{ten}\n"));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].popularity, 0, "a missing column is simply unranked");
        assert_eq!(items[0].exchange, None);
        assert_eq!(items[1].popularity, 7);
    }

    #[test]
    fn the_generated_listings_carry_their_ranking() {
        // Guards the seam between tools/build_listings.py and this parser: a
        // column written in the wrong place would read as zero everywhere and
        // quietly cost search its ordering, with nothing else going wrong.
        let items = parse_seed(include_str!("listings.tsv"));
        let ranked = items.iter().filter(|i| i.popularity > 0).count();
        assert!(
            ranked > items.len() / 2,
            "only {ranked} of {} listings are ranked",
            items.len()
        );
        let apple = items.iter().find(|i| i.symbol == "AAPL").expect("no AAPL");
        assert_eq!(apple.popularity, 9, "the largest company should top the scale");
    }

    #[test]
    fn names_are_searchable() {
        let idx = index();
        let hits = idx.search("apple", 5);
        assert_eq!(idx.get(hits[0].index).unwrap().symbol, "AAPL");

        let hits = idx.search("gold", 5);
        assert!(!hits.is_empty());
    }

    #[test]
    fn an_empty_query_shows_the_majors() {
        let idx = index();
        let hits = idx.search("", 10);
        assert_eq!(hits.len(), 10);
        assert!(hits.iter().all(|h| idx.get(h.index).unwrap().tier == 0));
    }

    #[test]
    fn well_known_tickers_beat_obscure_ones() {
        let idx = index();
        // Each of these is typed constantly and must win its prefix outright.
        for (query, expected) in [
            ("gc", "GC"),     // gold futures, not a microcap sharing the letters
            ("es", "ES"),     // E-mini S&P
            ("nq", "NQ"),
            ("cl", "CL"),
            ("sp", "SPY"),    // the ETF people mean when they type "sp"
            ("vi", "VIX"),
            ("bt", "BTC"),
            ("eur", "EURUSD"),
        ] {
            let hits = idx.search(query, 5);
            assert!(!hits.is_empty(), "no hits for {query}");
            assert_eq!(
                idx.get(hits[0].index).unwrap().symbol,
                expected,
                "{query} should surface {expected}, got {:?}",
                hits.iter().map(|h| &idx.get(h.index).unwrap().symbol).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn full_names_are_searchable_too() {
        let idx = index();
        for (query, expected) in [
            ("gold", "GC"),
            ("nvidia", "NVDA"),
            ("bitcoin", "BTC"),
            ("crude", "CL"),
            ("santander", "SAN"),
            ("nasdaq 100", "NDX"),
            ("volatility", "VIX"),
        ] {
            let hits = idx.search(query, 8);
            assert!(!hits.is_empty(), "no hits for {query}");
            let symbols: Vec<&str> =
                hits.iter().map(|h| idx.get(h.index).unwrap().symbol.as_str()).collect();
            assert!(symbols.contains(&expected), "{query} -> {symbols:?}, wanted {expected}");
        }
    }

    #[test]
    fn a_tier_zero_match_outranks_a_better_textual_tier_two_match() {
        // "micro" prefixes several tier-2 futures; the point is only that
        // ranking never puts an obscure instrument above a curated major when
        // the textual quality is comparable.
        let idx = index();
        let hits = idx.search("s", 10);
        let top: Vec<u8> = hits.iter().take(3).map(|h| idx.get(h.index).unwrap().tier).collect();
        assert!(top.iter().all(|&t| t <= 1), "top of 's' was tiers {top:?}");
    }

    #[test]
    fn search_is_fast_enough_to_run_on_every_keystroke() {
        let idx = index();
        let queries = ["a", "ap", "app", "appl", "g", "gc", "gol", "e", "es", "spy"];
        let start = std::time::Instant::now();
        let rounds = 200;
        for _ in 0..rounds {
            for q in queries {
                let _ = idx.search(q, 20);
            }
        }
        let per_query = start.elapsed() / (rounds * queries.len() as u32);
        // The budget is a millisecond; anything near it means the index
        // regressed into a full scan.
        assert!(per_query < std::time::Duration::from_micros(500), "{per_query:?} per query");
    }

    #[test]
    fn only_the_round_the_clock_markets_are_continuous() {
        assert!(InstrumentKind::Fx.is_continuous());
        assert!(InstrumentKind::Crypto.is_continuous());
        for kind in [
            InstrumentKind::Equity,
            InstrumentKind::Etf,
            InstrumentKind::Index,
            InstrumentKind::FutureRoot,
        ] {
            assert!(!kind.is_continuous(), "{kind:?}");
        }
    }

    #[test]
    fn exact_lookup_distinguishes_listings() {
        let idx = index();
        assert_eq!(idx.find("AAPL", None).unwrap().name, "Apple");
        assert_eq!(idx.find("SAN", Some("MC")).unwrap().name, "Banco Santander");
        // The Madrid listing must not answer a lookup for a US one.
        assert!(idx.find("SAN", None).is_none());
        assert!(idx.find("NOPE", None).is_none());
    }

    #[test]
    fn a_pasted_list_reads_however_it_was_separated() {
        assert_eq!(tickers("aapl, msft;NVDA\r\n  amd\tTSM  "), ["AAPL", "MSFT", "NVDA", "AMD", "TSM"]);
        assert_eq!(tickers("\"AAPL\",\"MSFT\""), ["AAPL", "MSFT"], "a CSV column");
        assert_eq!(tickers("AAPL, aapl, AAPL"), ["AAPL"], "each once");
        assert!(tickers(" , ;\n").is_empty());
    }

    /// A watchlist export of the common shape: one line, exchange-prefixed,
    /// with its sections as `###` markers between the symbols.
    #[test]
    fn an_exported_watchlist_reads_as_its_tickers() {
        let export = "###Mega caps,NASDAQ:AAPL,NASDAQ:MSFT,###Banks,NYSE:JPM";
        assert_eq!(tickers(export), ["AAPL", "MSFT", "JPM"], "a section name is not a ticker");
    }

    #[test]
    fn a_list_resolves_to_instruments_and_says_what_it_could_not() {
        let idx = index();
        let (found, unknown) = idx.resolve_list("AAPL, SAN.MC, NOTATICKER, aapl, ES", None);
        let found: Vec<String> = found.iter().map(|i| i.display_symbol()).collect();
        assert_eq!(found, ["AAPL", "SAN.MC", "ES"], "in the order given, each once");
        assert_eq!(unknown, ["NOTATICKER"]);

        let (found, _) = idx.resolve_list("SAN", Some("MC"));
        assert_eq!(found[0].display_symbol(), "SAN.MC", "a venue for tickers given without one");
    }

    #[test]
    fn a_dot_is_part_of_the_ticker_before_it_is_a_venue() {
        let idx = index();
        assert_eq!(idx.lookup("FTSEMIB.MI").map(|i| i.symbol.as_str()), Some("FTSEMIB.MI"));
        assert_eq!(idx.lookup("SAN.MC").map(|i| i.display_symbol()).as_deref(), Some("SAN.MC"));
        assert!(idx.lookup("SAN.XX").is_none());
    }

    #[test]
    fn nonsense_matches_nothing() {
        assert!(index().search("zzzzqq", 5).is_empty());
    }
}
