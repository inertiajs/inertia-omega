//! Validation errors, shared with the client through the `errors` prop.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Validation error messages, by field.
///
/// ```
/// use inertia::ValidationErrors;
///
/// let errors = ValidationErrors::new()
///     .with("email", "The email field is required.")
///     .with("name", "The name must be at least 3 characters.");
///
/// assert_eq!(errors.first("email"), Some("The email field is required."));
/// ```
///
/// With the `validator` or `garde` features, their error types convert into
/// `ValidationErrors` with `From`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[must_use]
pub struct ValidationErrors(IndexMap<String, Vec<String>>);

impl ValidationErrors {
    /// Create an empty set of errors.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a message for the given field.
    pub fn add(&mut self, field: impl Into<String>, message: impl Into<String>) -> &mut Self {
        self.0.entry(field.into()).or_default().push(message.into());
        self
    }

    /// Add a message for the given field, returning the errors for chaining.
    pub fn with(mut self, field: impl Into<String>, message: impl Into<String>) -> Self {
        self.add(field, message);
        self
    }

    /// Whether there are no errors.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the given field has errors.
    pub fn has(&self, field: &str) -> bool {
        self.0.contains_key(field)
    }

    /// The first message for the given field.
    pub fn first(&self, field: &str) -> Option<&str> {
        self.0.get(field)?.first().map(String::as_str)
    }

    /// The messages for the given field.
    pub fn get(&self, field: &str) -> &[String] {
        self.0.get(field).map_or(&[], Vec::as_slice)
    }

    /// The fields and their messages.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.0
            .iter()
            .map(|(field, messages)| (field.as_str(), messages.as_slice()))
    }

    /// Add all messages of another set of errors.
    pub fn merge(&mut self, other: ValidationErrors) {
        for (field, messages) in other.0 {
            self.0.entry(field).or_default().extend(messages);
        }
    }

    /// The errors as the client expects them: the first message per field,
    /// or every message when `all` is set.
    fn to_value(&self, all: bool) -> Value {
        let fields = self.0.iter().map(|(field, messages)| {
            let messages = if all {
                Value::from(messages.clone())
            } else {
                Value::from(messages.first().cloned().unwrap_or_default())
            };

            (field.clone(), messages)
        });

        Value::Object(fields.collect())
    }
}

impl<F: Into<String>, M: Into<String>> FromIterator<(F, M)> for ValidationErrors {
    fn from_iter<I: IntoIterator<Item = (F, M)>>(iter: I) -> Self {
        let mut errors = Self::new();
        for (field, message) in iter {
            errors.add(field, message);
        }
        errors
    }
}

/// Validation errors by error bag, as kept in the session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ErrorBags(IndexMap<String, ValidationErrors>);

impl ErrorBags {
    pub(crate) fn add(&mut self, bag: impl Into<String>, errors: ValidationErrors) {
        self.0.entry(bag.into()).or_default().merge(errors);
    }

    pub(crate) fn merge(&mut self, other: ErrorBags) {
        for (bag, errors) in other.0 {
            self.add(bag, errors);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.iter().all(|(_, errors)| errors.is_empty())
    }

    /// The `errors` prop: the default bag on its own (scoped under the error
    /// bag the client asked for, if any), or every bag by name.
    pub(crate) fn to_prop(&self, error_bag: Option<&str>, all: bool) -> Value {
        if let Some(default) = self.0.get("default") {
            let default = default.to_value(all);

            return match error_bag {
                Some(bag) => Value::Object(Map::from_iter([(bag.to_owned(), default)])),
                None => default,
            };
        }

        Value::Object(
            self.0
                .iter()
                .map(|(bag, errors)| (bag.clone(), errors.to_value(all)))
                .collect(),
        )
    }
}

#[cfg(feature = "validator")]
impl From<validator::ValidationErrors> for ValidationErrors {
    /// Nested structs' and lists' errors are keyed by their dotted path, as
    /// Laravel keys them (`items.1.name`). Fields are in alphabetical order,
    /// since `validator` keeps them in a `HashMap`.
    fn from(errors: validator::ValidationErrors) -> Self {
        fn collect(prefix: &str, errors: &validator::ValidationErrors, into: &mut ValidationErrors) {
            use validator::ValidationErrorsKind;

            let mut fields: Vec<_> = errors.errors().iter().collect();
            fields.sort_by_key(|(field, _)| *field);

            for (field, kind) in fields {
                let path = if prefix.is_empty() {
                    field.to_string()
                } else {
                    format!("{prefix}.{field}")
                };

                match kind {
                    ValidationErrorsKind::Field(errors) => {
                        for error in errors {
                            let message = error
                                .message
                                .as_ref()
                                .map_or_else(|| invalid(&path), ToString::to_string);
                            into.add(path.clone(), message);
                        }
                    }
                    ValidationErrorsKind::Struct(errors) => collect(&path, errors, into),
                    ValidationErrorsKind::List(items) => {
                        for (index, errors) in items {
                            collect(&format!("{path}.{index}"), errors, into);
                        }
                    }
                }
            }
        }

        let mut into = Self::new();
        collect("", &errors, &mut into);
        into
    }
}

/// The message for a `validator` rule declared without one: its code alone
/// (`length`) means nothing to a user.
#[cfg(feature = "validator")]
fn invalid(path: &str) -> String {
    format!("The {} field is invalid.", path.replace('_', " "))
}

#[cfg(feature = "garde")]
impl From<garde::Report> for ValidationErrors {
    /// Paths are dotted, as Laravel keys them: garde's `items[1].name` is
    /// `items.1.name`.
    fn from(report: garde::Report) -> Self {
        report
            .iter()
            .map(|(path, error)| (dotted(&path.to_string()), error.message().to_string()))
            .collect()
    }
}

#[cfg(feature = "garde")]
fn dotted(path: &str) -> String {
    path.replace('[', ".")
        .replace(']', "")
        .trim_start_matches('.')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn bags() -> ErrorBags {
        let mut bags = ErrorBags::default();
        bags.add(
            "default",
            ValidationErrors::from_iter([("name", "Required."), ("name", "Too short.")]),
        );
        bags
    }

    #[test]
    fn the_default_bag_is_shared_on_its_own() {
        assert_eq!(bags().to_prop(None, false), json!({ "name": "Required." }));
        assert_eq!(
            bags().to_prop(None, true),
            json!({ "name": ["Required.", "Too short."] })
        );
    }

    #[test]
    fn the_default_bag_is_scoped_to_the_requested_error_bag() {
        assert_eq!(
            bags().to_prop(Some("createUser"), false),
            json!({ "createUser": { "name": "Required." } })
        );
    }

    #[test]
    fn named_bags_are_shared_by_name() {
        let mut bags = ErrorBags::default();
        bags.add("login", ValidationErrors::new().with("email", "Invalid."));

        assert_eq!(bags.to_prop(None, false), json!({ "login": { "email": "Invalid." } }));
    }
}
