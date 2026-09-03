//! `CREATE CUBE` and `DROP CUBE`.
//!
//! # Why a cube needed a statement at all
//!
//! Until this module a cube reached a warehouse exactly one way: somebody wrote a JSON file
//! into `<warehouse>/_cubes/` and restarted the server, which
//! [`catalogue`](sankhya_cube::catalogue) documents as *"deliberately the smaller thing"*. It
//! was, and it was enough to prove the model. It is not enough to *use*, and the gap shows up
//! somewhere specific: `M12`'s twelve-hour run builds, queries and drops cuboids underneath a
//! live workload, and it cannot do that through a directory and a restart. A client has to be
//! able to say it.
//!
//! # Why a parser here rather than a DataFusion extension
//!
//! `CREATE CUBE` is not SQL, so `sqlparser` rejects it before any DataFusion hook can see it,
//! and the extension points that do exist (`QueryPlanner`, `ExprPlanner`) all sit *downstream*
//! of a successful parse. The statement therefore has to be recognised before the engine is
//! asked, which is what [`parse`] is for.
//!
//! That places one hard requirement on this module, and it is the reason [`parse`] returns an
//! `Option` rather than a `Result`: **it must be able to say "not mine" without opinion.**
//! Anything that is not cube DDL has to reach the engine untouched, including statements that
//! merely mention a cube, and including malformed SQL — whose error must come from the engine
//! that owns the language rather than from a pre-filter that happened to look first.
//!
//! # Nothing here validates a cube
//!
//! This module produces a [`Definition`], never a `Cube`. Turning one into the other is
//! [`Definition::validate`], which returns **every** rejection rather than the first, and which
//! already refuses an undeclared measure rule, a cyclic hierarchy and a dimension with no
//! levels. A second implementation of those rules here would be a second implementation to
//! disagree with the first, and the disagreement would surface as a cube that a file can
//! declare and a statement cannot.
//!
//! So the split is: this module decides **what was written**, and `validate` decides **whether
//! it describes a cube**. A syntax error names a position; a rejected cube names a rule.

use sankhya_cube::model::{Definition, Dimension, Level};
use sankhya_cube_algo::hierarchy::Hierarchy;
use sankhya_cube_algo::measure::{Along, Measure, Rule};
use std::fmt;

/// A cube-DDL statement.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Statement {
    /// Create a cube from a definition that has been read but **not** validated.
    Create(Box<Definition>),
    /// Drop a cube by name.
    Drop {
        /// The cube named.
        name: String,
        /// Whether the statement said `IF EXISTS`.
        ///
        /// Carried rather than resolved here, because whether the cube exists is a question
        /// about a warehouse and this module has never seen one.
        if_exists: bool,
    },
}

/// Why a statement that began as cube DDL could not be read.
///
/// Every variant carries the byte offset the reader had reached, because a cube definition is
/// long enough that "unexpected token" without a position is a hunt.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DdlError {
    /// What was expected, in the words a writer would use.
    pub expected: String,
    /// What was found, or `None` at the end of the statement.
    pub found: Option<String>,
    /// Byte offset into the statement.
    pub at: usize,
}

impl fmt::Display for DdlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.found {
            Some(found) => write!(
                f,
                "expected {} at offset {}, found `{found}`",
                self.expected, self.at
            ),
            None => write!(
                f,
                "expected {} at offset {}, and the statement ended",
                self.expected, self.at
            ),
        }
    }
}

impl std::error::Error for DdlError {}

/// Read a cube-DDL statement, if this is one.
///
/// Returns `None` when the statement is not cube DDL, which is not an error and must not be
/// reported as one — it is how everything else in the language reaches the engine. `Some(Err)`
/// means the statement opened as cube DDL and then did not parse, which is a syntax error the
/// writer wants to see rather than a statement to pass along: `CREATE CUBE` followed by
/// nonsense is not a query DataFusion can make sense of either, and forwarding it would report
/// the wrong error at the wrong layer.
///
/// # Errors
///
/// When the statement begins `CREATE CUBE` or `DROP CUBE` and does not parse.
pub fn parse(sql: &str) -> Option<Result<Statement, DdlError>> {
    let mut reader = Reader::new(sql);
    let opener = reader.peek_words(2)?;
    match (opener.0.to_ascii_uppercase().as_str(), opener.1.to_ascii_uppercase().as_str()) {
        ("CREATE", "CUBE") => Some(reader.create()),
        ("DROP", "CUBE") => Some(reader.drop()),
        _ => None,
    }
}

/// A token: either a word, a number, or a single punctuation character.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Token {
    /// A bare or double-quoted identifier, or a keyword. Quoted words keep their case and
    /// are never matched against a keyword.
    Word { text: String, quoted: bool },
    Number(u64),
    Punct(char),
}

impl Token {
    fn shown(&self) -> String {
        match self {
            Self::Word { text, .. } => text.clone(),
            Self::Number(n) => n.to_string(),
            Self::Punct(c) => c.to_string(),
        }
    }
}

/// A token and where it started.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Spanned {
    token: Token,
    at: usize,
}

/// A cursor over the statement's tokens.
struct Reader {
    tokens: Vec<Spanned>,
    next: usize,
    /// The offset just past the last token, for errors raised at the end.
    end: usize,
    /// The statement as it was written, so a declared query can be taken back out of it
    /// verbatim. The tokeniser throws away whitespace and quoting, and a query handed to the
    /// engine has to be the text somebody wrote --- not a re-rendering of it.
    sql: String,
}

impl Reader {
    fn new(sql: &str) -> Self {
        let tokens = tokenize(sql);
        Self { tokens, next: 0, end: sql.len(), sql: sql.to_owned() }
    }

    /// A parenthesised run of the statement, returned with its parentheses, or `None` if the
    /// next token is not an opening one.
    ///
    /// Matched over the **raw text** rather than over tokens, because a `(` inside a string
    /// literal is a character and not a nesting level: `WHERE label = '('` would otherwise
    /// leave the depth counter one too deep and swallow the rest of the statement. The token
    /// cursor is then advanced past everything inside the span.
    fn parenthesised(&mut self) -> Option<Result<String, DdlError>> {
        let opens = self.peek()?.at;
        if !matches!(self.peek().map(|s| &s.token), Some(Token::Punct('('))) {
            return None;
        }
        let mut depth = 0usize;
        let mut quoted = false;
        let mut closes = None;
        for (at, c) in self.sql.char_indices().skip_while(|(at, _)| *at < opens) {
            if quoted {
                // A doubled quote is an escaped one and leaves the literal open, which the
                // next iteration sees as a fresh opening quote. Same outcome, no lookahead.
                if c == '\'' {
                    quoted = false;
                }
                continue;
            }
            match c {
                '\'' => quoted = true,
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        closes = Some(at);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(closes) = closes else {
            return Some(Err(DdlError {
                expected: "a closing `)` for the fact query".to_owned(),
                found: None,
                at: opens,
            }));
        };
        // Past the span, whatever it contained. A token whose offset is inside the query
        // belongs to the query, not to the cube's own grammar.
        while self.peek().is_some_and(|spanned| spanned.at <= closes) {
            self.next += 1;
        }
        Some(Ok(self.sql.get(opens..=closes).unwrap_or_default().to_owned()))
    }

    /// The first `n` words, without consuming anything.
    ///
    /// Used only to decide whether this statement is ours. A statement whose first two tokens
    /// are not both words is certainly not, so this returning `None` is the common case.
    fn peek_words(&self, n: usize) -> Option<(String, String)> {
        if n != 2 {
            return None;
        }
        let first = self.tokens.first()?;
        let second = self.tokens.get(1)?;
        match (&first.token, &second.token) {
            (
                Token::Word { text: a, quoted: false },
                Token::Word { text: b, quoted: false },
            ) => Some((a.clone(), b.clone())),
            _ => None,
        }
    }

    fn peek(&self) -> Option<&Spanned> {
        self.tokens.get(self.next)
    }

    fn at(&self) -> usize {
        self.peek().map_or(self.end, |spanned| spanned.at)
    }

    fn fail<T>(&self, expected: &str) -> Result<T, DdlError> {
        Err(DdlError {
            expected: expected.to_string(),
            found: self.peek().map(|spanned| spanned.token.shown()),
            at: self.at(),
        })
    }

    /// Consume a keyword, case-insensitively. A quoted word is never a keyword.
    fn keyword(&mut self, word: &str) -> Result<(), DdlError> {
        match self.peek().map(|spanned| &spanned.token) {
            Some(Token::Word { text, quoted: false })
                if text.eq_ignore_ascii_case(word) =>
            {
                self.next += 1;
                Ok(())
            }
            _ => self.fail(&format!("`{}`", word.to_ascii_uppercase())),
        }
    }

    /// Whether the next token is this keyword, without consuming it.
    fn peek_keyword(&self, word: &str) -> bool {
        matches!(
            self.peek().map(|spanned| &spanned.token),
            Some(Token::Word { text, quoted: false }) if text.eq_ignore_ascii_case(word)
        )
    }

    /// Consume this keyword if it is next, and say whether it was.
    fn optional_keyword(&mut self, word: &str) -> bool {
        let there = self.peek_keyword(word);
        if there {
            self.next += 1;
        }
        there
    }

    fn punct(&mut self, want: char) -> Result<(), DdlError> {
        match self.peek().map(|spanned| &spanned.token) {
            Some(Token::Punct(found)) if *found == want => {
                self.next += 1;
                Ok(())
            }
            _ => self.fail(&format!("`{want}`")),
        }
    }

    fn peek_punct(&self, want: char) -> bool {
        matches!(self.peek().map(|s| &s.token), Some(Token::Punct(c)) if *c == want)
    }

    /// A name: any word, keyword or not.
    ///
    /// Keywords are **not** reserved. A cube called `level` or a column called `sum` is a
    /// perfectly ordinary thing for somebody to have, and a grammar that refuses it buys
    /// nothing here: every position that takes a name is one where a keyword cannot also
    /// appear, so there is nothing to disambiguate.
    fn name(&mut self, what: &str) -> Result<String, DdlError> {
        match self.peek().map(|spanned| spanned.token.clone()) {
            Some(Token::Word { text, .. }) => {
                self.next += 1;
                Ok(text)
            }
            _ => self.fail(what),
        }
    }

    /// A table name, which may name its schema: `orders` or `sales.orders`.
    ///
    /// # Why this is separate from [`Self::name`]
    ///
    /// The lexer stops an unquoted word at a `.`, so `sales.orders` arrived here as three
    /// tokens and a cube could not name a table in a schema **at all** --- the qualified form
    /// failed to parse and the bare form resolved only while one schema claimed the name. On a
    /// warehouse with more than one schema, which is every real one, that made the whole cube
    /// surface unreachable.
    ///
    /// Only the two table positions use this. A dimension name, a level name and a column name
    /// are not qualified, and accepting a dot there would parse a typo into a name nothing
    /// resolves.
    fn qualified_name(&mut self, what: &str) -> Result<String, DdlError> {
        let first = self.name(what)?;
        if self.peek().map(|spanned| &spanned.token) != Some(&Token::Punct('.')) {
            return Ok(first);
        }
        self.next += 1;
        let second = self.name(what)?;
        Ok(format!("{first}.{second}"))
    }

    fn number(&mut self, what: &str) -> Result<u64, DdlError> {
        match self.peek().map(|spanned| spanned.token.clone()) {
            Some(Token::Number(n)) => {
                self.next += 1;
                Ok(n)
            }
            _ => self.fail(what),
        }
    }

    /// `DROP CUBE [IF EXISTS] <name>`
    fn drop(&mut self) -> Result<Statement, DdlError> {
        self.keyword("DROP")?;
        self.keyword("CUBE")?;
        let if_exists = if self.peek_keyword("IF") {
            self.keyword("IF")?;
            self.keyword("EXISTS")?;
            true
        } else {
            false
        };
        let name = self.name("a cube name")?;
        self.end_of_statement()?;
        Ok(Statement::Drop { name, if_exists })
    }

    /// `CREATE CUBE <name> FROM <fact> <dimension>+ <measure>+ [MAINTAINED …] [PINNED …]*`
    fn create(&mut self) -> Result<Statement, DdlError> {
        self.keyword("CREATE")?;
        self.keyword("CUBE")?;
        let name = self.name("a cube name")?;
        self.keyword("FROM")?;
        // A name, or a parenthesised query. `ADR-0012`: a cube's fact source becomes a
        // declared query rather than a name, and everything else about a cube is unchanged.
        // Which tables that query reads is not decided here --- it needs a catalogue and the
        // caller's guard, and this parser has neither.
        let fact_query = match self.parenthesised() {
            Some(query) => Some(query?),
            None => None,
        };
        let fact_table = match &fact_query {
            Some(query) => query.clone(),
            None => self.qualified_name("the fact table's name")?,
        };

        let mut dimensions = Vec::new();
        while self.peek_keyword("DIMENSION") {
            dimensions.push(self.dimension()?);
        }
        let mut measures = Vec::new();
        while self.peek_keyword("MEASURE") {
            measures.push(self.measure()?);
        }
        // Said here rather than left to `validate`, because the message a writer needs is
        // about the statement they typed. `validate` refuses a cube with no dimensions too,
        // and phrases it as a property of the cube; at this point the useful sentence is that
        // the clause is missing and what it looks like.
        if dimensions.is_empty() {
            return self.fail("at least one `DIMENSION <name> FROM <table> ON <column> (…)`");
        }
        if measures.is_empty() {
            return self.fail("at least one `MEASURE <name> (<RULE> ALONG <dimension>, …)`");
        }

        let mut definition = Definition::new(name, fact_table, dimensions, measures);
        if fact_query.is_some() {
            // Nothing yet, and said so: a query reads whatever it reads, and until somebody
            // resolves it the cube is refused by `validate` rather than accepted with a
            // dependency list that claims it reads its own text.
            definition.reads = Vec::new();
        }
        if self.optional_keyword("MAINTAINED") {
            self.keyword("WITHIN")?;
            let versions = self.number("a number of versions")?;
            // `VERSIONS` and `VERSION` both, because `WITHIN 1 VERSIONS` reads badly enough
            // that somebody will write the singular and be right to.
            if !self.optional_keyword("VERSIONS") {
                self.keyword("VERSION")?;
            }
            definition = definition.maintained_within(versions);
        }
        while self.peek_keyword("PINNED") {
            self.keyword("PINNED")?;
            self.punct('(')?;
            let mut pinned = Vec::new();
            loop {
                pinned.push(self.name("a dimension name")?);
                if !self.peek_punct(',') {
                    break;
                }
                self.punct(',')?;
            }
            self.punct(')')?;
            definition = definition.pinning(pinned);
        }

        self.end_of_statement()?;
        Ok(Statement::Create(Box::new(definition)))
    }

    /// `DIMENSION <name> FROM <table> ON <fact column> ( <item>, … )`
    fn dimension(&mut self) -> Result<Dimension, DdlError> {
        self.keyword("DIMENSION")?;
        let name = self.name("a dimension name")?;
        self.keyword("FROM")?;
        let table = self.qualified_name("the dimension table's name")?;
        self.keyword("ON")?;
        let joins_on = self.name("the fact-table column that joins to it")?;
        self.punct('(')?;

        let mut levels = Vec::new();
        let mut rollups: Option<Hierarchy> = None;
        let mut parent_child = None;
        loop {
            if self.peek_keyword("LEVEL") {
                self.keyword("LEVEL")?;
                let level = self.name("a level name")?;
                self.punct('=')?;
                let column = self.name("the column holding its member key")?;
                levels.push(Level::new(level, column));
            } else if self.peek_keyword("PARENT") {
                self.keyword("PARENT")?;
                let child = self.name("the child column")?;
                self.keyword("TO")?;
                let parent = self.name("the parent column")?;
                parent_child = Some((child, parent));
            } else if self.peek_keyword("ROLLUP") {
                self.keyword("ROLLUP")?;
                let child = self.name("the child member")?;
                self.keyword("TO")?;
                let parent = self.name("the parent member")?;
                rollups.get_or_insert_with(Hierarchy::new).rolls_up(child, parent);
            } else {
                return self.fail("`LEVEL`, `PARENT` or `ROLLUP`");
            }
            if !self.peek_punct(',') {
                break;
            }
            self.punct(',')?;
        }
        self.punct(')')?;

        let mut dimension = Dimension::new(name, table, joins_on, levels);
        dimension.rollups = rollups;
        dimension.parent_child = parent_child;
        Ok(dimension)
    }

    /// `MEASURE <name> ( <RULE> ALONG <dimension>, … )`
    fn measure(&mut self) -> Result<Measure, DdlError> {
        self.keyword("MEASURE")?;
        let name = self.name("a measure name")?;
        self.punct('(')?;
        let mut rules = Vec::new();
        loop {
            let rule = self.rule()?;
            self.keyword("ALONG")?;
            let dimension = self.name("a dimension name")?;
            rules.push(Along::new(dimension, rule));
            if !self.peek_punct(',') {
                break;
            }
            self.punct(',')?;
        }
        self.punct(')')?;
        Ok(Measure::new(name, rules))
    }

    /// One additivity rule.
    ///
    /// `NONE` is spelled out and is not a default. A measure that cannot be derived from its
    /// children is a real and common thing — a ratio, a distinct count — and the whole design
    /// of the measure model is that it is *declared* rather than assumed.
    fn rule(&mut self) -> Result<Rule, DdlError> {
        let Some(Token::Word { text, .. }) = self.peek().map(|spanned| spanned.token.clone())
        else {
            return self.fail("a rule: `SUM`, `LAST`, `FIRST`, `MAX`, `MIN`, `MEAN` or `NONE`");
        };
        let rule = match text.to_ascii_uppercase().as_str() {
            "SUM" => Rule::Sum,
            "LAST" => Rule::Last,
            "FIRST" => Rule::First,
            "MAX" => Rule::Max,
            "MIN" => Rule::Min,
            "MEAN" => Rule::Mean,
            "NONE" => Rule::None,
            _ => {
                return self
                    .fail("a rule: `SUM`, `LAST`, `FIRST`, `MAX`, `MIN`, `MEAN` or `NONE`")
            }
        };
        self.next += 1;
        Ok(rule)
    }

    /// Nothing but an optional terminating semicolon may follow.
    ///
    /// Checked rather than ignored. Trailing tokens mean the writer intended something the
    /// grammar did not take — a misspelled clause, a second statement — and silently dropping
    /// them would create a cube subtly unlike the one that was asked for.
    fn end_of_statement(&mut self) -> Result<(), DdlError> {
        if self.peek_punct(';') {
            self.punct(';')?;
        }
        if self.peek().is_some() {
            return self.fail("the end of the statement");
        }
        Ok(())
    }
}

/// Split a statement into words, numbers and single punctuation characters.
///
/// Comments are not handled, and that is deliberate rather than pending: this tokenizer sees a
/// statement only after [`parse`] has decided it opens with `CREATE CUBE` or `DROP CUBE`, and a
/// leading comment would have stopped that. A comment *inside* a definition is worth having and
/// is worth doing properly, in the one tokenizer the whole language shares, rather than twice.
fn tokenize(sql: &str) -> Vec<Spanned> {
    let mut tokens = Vec::new();
    let bytes: Vec<(usize, char)> = sql.char_indices().collect();
    let mut i = 0;
    while let Some(&(at, c)) = bytes.get(i) {
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' {
            // A quoted identifier keeps its case and is never a keyword, which is what makes
            // a cube called "Level" expressible.
            let mut text = String::new();
            i += 1;
            while let Some(&(_, c)) = bytes.get(i) {
                if c == '"' {
                    i += 1;
                    break;
                }
                text.push(c);
                i += 1;
            }
            tokens.push(Spanned { token: Token::Word { text, quoted: true }, at });
        } else if c.is_ascii_digit() {
            let mut text = String::new();
            while let Some(&(_, c)) = bytes.get(i) {
                if !c.is_ascii_digit() {
                    break;
                }
                text.push(c);
                i += 1;
            }
            // A number too large for the staleness target is kept as a word rather than
            // saturated. Saturating would accept `MAINTAINED WITHIN 99999999999999999999999
            // VERSIONS` as some other number entirely, which is the shape of mistake that is
            // worth a message.
            match text.parse::<u64>() {
                Ok(n) => tokens.push(Spanned { token: Token::Number(n), at }),
                Err(_) => {
                    tokens.push(Spanned { token: Token::Word { text, quoted: false }, at });
                }
            }
        } else if c.is_alphanumeric() || c == '_' {
            let mut text = String::new();
            while let Some(&(_, c)) = bytes.get(i) {
                if !(c.is_alphanumeric() || c == '_') {
                    break;
                }
                text.push(c);
                i += 1;
            }
            tokens.push(Spanned { token: Token::Word { text, quoted: false }, at });
        } else {
            tokens.push(Spanned { token: Token::Punct(c), at });
            i += 1;
        }
    }
    tokens
}
