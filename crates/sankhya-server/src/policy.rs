//! The policy an operator writes, read from configuration.
//!
//! # What was wrong
//!
//! `start()` --- the only path the shipped binary takes --- built `permissive_policy`, which
//! grants `reader` read on every discovered table with no row filter and no mask. **No
//! configuration key loaded a policy set at all.** So the row-predicate enforcement, which
//! §13.2 spends four pages on and which is the best-tested code in the repository, had never
//! run outside a test. `SEC-15`.
//!
//! That is a different kind of finding from the rest of Phase 4. Nothing here was wrong; the
//! thing was simply not reachable, and every document describing it described a capability the
//! binary could not be configured into. A feature that ships unreachable is a feature that has
//! been paid for and not delivered.
//!
//! # The shape
//!
//! ```yaml
//! policy:
//!   rules:
//!     analysts_read_orders:
//!       role: analyst
//!       table: sales.orders
//!       action: read
//!       where: region = 'north'
//!       mask:
//!         email: null
//!         phone: partial:4
//! ```
//!
//! Each rule is **named**, and the name is the operator's. A list would have been shorter to
//! write and impossible to talk about: a refusal that says *"rule 3 does not parse"* is one
//! somebody has to count to, and a policy is the file people review line by line.
//!
//! # Why a qualified table name is required
//!
//! Because a policy is the one place an ambiguity is fatal. A bare `orders` means one table
//! today and two the day somebody adds a schema --- and the rule would then silently apply to
//! neither, since a contested bare name resolves nowhere. Refused with a sentence instead.
//!
//! # Why an absent policy is not an error
//!
//! Because a warehouse somebody is trying out has no policy and should still answer. What must
//! not happen is that it answers *the same way* as one that has been configured and says
//! nothing about it: the startup line names which of the two you have, capitalised, the same
//! way the authentication postures are.

use sankhya_authz::policy::{Action, Mask, PolicySet, Rule, TableRef};
use sankhya_authz::principal::{Role, TenantId};
use std::collections::BTreeMap;

/// Read the policy an operator configured, or `None` where they configured none.
///
/// # Errors
///
/// A rule missing a field it needs, naming an action or a mask this server does not have, or
/// naming a table without its schema. Every one is refused rather than skipped: a policy with a
/// rule quietly dropped is a policy that permits more than it says, and the operator who wrote
/// the rule believes it is in force.
pub fn read(
    settings: &BTreeMap<String, String>,
    tenant: TenantId,
) -> Result<Option<PolicySet>, String> {
    // Grouped by the name before the first dot, which is the rule's own name.
    let mut named: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (key, value) in settings {
        let Some((rule, field)) = key.split_once('.') else {
            return Err(format!(
                "`policy.rules.{key}` names no field. A rule is `policy.rules.<name>.<field>`, \
                 and a bare name with a value under it is a rule this server would have to \
                 guess the shape of"
            ));
        };
        named
            .entry(rule.to_owned())
            .or_default()
            .insert(field.to_owned(), value.clone());
    }
    if named.is_empty() {
        return Ok(None);
    }

    let mut policy = PolicySet::new();
    for (name, fields) in &named {
        policy = policy.with(one(name, fields, tenant)?);
    }
    Ok(Some(policy))
}

/// One rule, from the fields written under its name.
fn one(
    name: &str,
    fields: &BTreeMap<String, String>,
    tenant: TenantId,
) -> Result<Rule, String> {
    let need = |field: &str| -> Result<&String, String> {
        fields.get(field).ok_or_else(|| {
            format!(
                "the policy rule `{name}` has no `{field}`. A rule needs `role`, `table` and \
                 `action`; `effect`, `where` and `mask` are optional"
            )
        })
    };

    let table = need("table")?;
    let Some((schema, bare)) = table.split_once('.') else {
        return Err(format!(
            "the policy rule `{name}` names the table `{table}` without a schema. A policy is \
             the one place an ambiguous name is fatal: `orders` means one table today and two \
             the day somebody adds a schema, and the rule would then apply to neither"
        ));
    };
    if schema.is_empty() || bare.is_empty() {
        return Err(format!(
            "the policy rule `{name}` names the table `{table}`, which has an empty half"
        ));
    }

    let action = match need("action")?.to_lowercase().as_str() {
        "read" => Action::Read,
        "insert" => Action::Insert,
        "update" => Action::Update,
        "delete" => Action::Delete,
        other => {
            return Err(format!(
                "the policy rule `{name}` asks for the action `{other}`, and this server has \
                 `read`, `insert`, `update` and `delete`"
            ))
        }
    };

    let role = Role::new(need("role")?.clone());
    let reference = TableRef::new(schema, bare);
    // Deny beats any grant, which is why it is spelled out rather than inferred from an absent
    // grant: a rule that forbids is a decision somebody made, and it should be readable as one.
    let mut rule = match fields.get("effect").map(|effect| effect.to_lowercase()) {
        None => Rule::grant(tenant, role, reference, action),
        Some(effect) if effect == "grant" || effect == "allow" => {
            Rule::grant(tenant, role, reference, action)
        }
        Some(effect) if effect == "deny" || effect == "forbid" => {
            Rule::deny(tenant, role, reference, action)
        }
        Some(other) => {
            return Err(format!(
                "the policy rule `{name}` has the effect `{other}`, and this server has `grant` \
                 and `deny`"
            ))
        }
    };

    if let Some(predicate) = fields.get("where") {
        rule = rule.where_rows(predicate.clone());
    }
    for (field, value) in fields {
        let Some(column) = field.strip_prefix("mask.") else {
            continue;
        };
        rule = rule.masking(column.to_owned(), mask(name, column, value)?);
    }
    Ok(rule)
}

/// One column mask, from what an operator wrote against the column's name.
///
/// `null`, `constant:<text>`, `partial:<n>`. Written as a scalar rather than as a nested object
/// because the alternative --- `mask.email.kind: partial` and `mask.email.keep: 4` --- is two
/// keys that can disagree, and the disagreement would be a mask that silently became a
/// different mask.
fn mask(rule: &str, column: &str, written: &str) -> Result<Mask, String> {
    let refuse = |why: &str| {
        format!(
            "the policy rule `{rule}` masks `{column}` with `{written}`, and {why}. Write \
             `null`, `constant:<text>` or `partial:<how many characters to keep>`"
        )
    };
    match written.split_once(':') {
        // `null` and the empty value are the same mask, because YAML's `region: null` is a
        // *null scalar* and arrives here as nothing at all. Requiring `"null"` in quotes would
        // be a trap set for the one spelling an operator reaches for first --- and the two
        // cannot mean different things, since there is nothing else an empty mask could be.
        None if written.is_empty() || written.eq_ignore_ascii_case("null") => Ok(Mask::Null),
        None => Err(refuse("that is not a mask this server has")),
        Some((kind, rest)) if kind.eq_ignore_ascii_case("constant") => Ok(Mask::Constant {
            value: rest.to_owned(),
        }),
        Some((kind, rest)) if kind.eq_ignore_ascii_case("partial") => {
            let keep = rest
                .trim()
                .parse::<usize>()
                .map_err(|_| refuse("a partial mask keeps a number of characters"))?;
            Ok(Mask::Partial { keep })
        }
        Some((kind, _)) => Err(refuse(&format!("`{kind}` is not a kind of mask"))),
    }
}

#[cfg(test)]
mod tests {
    // A test chooses all of its data, so an assertion that cannot fail loudly is worse than
    // useless. The workspace denies these because a *server* must not do them to data it did
    // not choose --- see `crates/sankhya-catalog/tests/enforcement.rs` for the same note.
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::{read, TenantId};
    use std::collections::BTreeMap;

    fn tenant() -> TenantId {
        TenantId::from_uuid(uuid::Uuid::from_u128(1))
    }

    fn written(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_warehouse_with_no_policy_configured_has_none() {
        assert!(read(&written(&[]), tenant()).expect("no policy is not an error").is_none());
    }

    #[test]
    fn a_rule_becomes_a_rule() {
        let policy = read(
            &written(&[
                ("analysts.role", "analyst"),
                ("analysts.table", "sales.orders"),
                ("analysts.action", "read"),
                ("analysts.where", "region = 'north'"),
                ("analysts.mask.email", "null"),
                ("analysts.mask.phone", "partial:4"),
            ]),
            tenant(),
        )
        .expect("a well-formed rule")
        .expect("a policy");
        assert_eq!(policy.len(), 1);
    }

    /// Every one of these is refused rather than skipped. A policy with a rule quietly dropped
    /// permits more than it says, and the person who wrote the rule believes it is in force.
    #[test]
    fn a_rule_that_cannot_be_read_is_refused_and_says_why() {
        for (why, pairs) in [
            ("no role", vec![("r.table", "sales.orders"), ("r.action", "read")]),
            ("no table", vec![("r.role", "analyst"), ("r.action", "read")]),
            ("no action", vec![("r.role", "analyst"), ("r.table", "sales.orders")]),
            (
                "an unqualified table",
                vec![("r.role", "a"), ("r.table", "orders"), ("r.action", "read")],
            ),
            (
                "an action this server does not have",
                vec![("r.role", "a"), ("r.table", "s.o"), ("r.action", "truncate")],
            ),
            (
                "an effect this server does not have",
                vec![
                    ("r.role", "a"),
                    ("r.table", "s.o"),
                    ("r.action", "read"),
                    ("r.effect", "maybe"),
                ],
            ),
            (
                "a mask this server does not have",
                vec![
                    ("r.role", "a"),
                    ("r.table", "s.o"),
                    ("r.action", "read"),
                    ("r.mask.email", "hash"),
                ],
            ),
            (
                "a partial mask that keeps no number",
                vec![
                    ("r.role", "a"),
                    ("r.table", "s.o"),
                    ("r.action", "read"),
                    ("r.mask.email", "partial:some"),
                ],
            ),
        ] {
            let refused = read(&written(&pairs), tenant())
                .expect_err(&format!("a rule with {why} must be refused"));
            assert!(
                !refused.is_empty() && refused.contains('`'),
                "and must name what it could not read: {refused}"
            );
        }
    }

    /// `null` and nothing are one mask, because YAML's `region: null` arrives as nothing.
    #[test]
    fn the_two_spellings_of_a_null_mask_agree() {
        for spelling in ["null", "NULL", ""] {
            let policy = read(
                &written(&[
                    ("r.role", "a"),
                    ("r.table", "s.o"),
                    ("r.action", "read"),
                    ("r.mask.email", spelling),
                ]),
                tenant(),
            );
            assert!(policy.is_ok(), "`{spelling}` is a null mask");
        }
    }

    #[test]
    fn a_denial_is_spelled_out_rather_than_inferred() {
        let policy = read(
            &written(&[
                ("nobody.role", "intern"),
                ("nobody.table", "hr.payroll"),
                ("nobody.action", "read"),
                ("nobody.effect", "deny"),
            ]),
            tenant(),
        )
        .expect("a well-formed rule")
        .expect("a policy");
        assert_eq!(policy.len(), 1);
    }
}
