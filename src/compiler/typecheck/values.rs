use super::split_top_level;

#[derive(Clone, Default)]
pub(super) struct ValuePack {
    pub(super) fixed: Vec<String>,
    pub(super) variadic: Option<String>,
}

impl ValuePack {
    pub(super) fn single(value: String) -> Self {
        Self {
            fixed: vec![value],
            variadic: None,
        }
    }

    pub(super) fn from_type(value: &str) -> Self {
        let Some(tuple) = value
            .trim()
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
        else {
            return Self::single(value.to_owned());
        };
        let fixed = split_top_level(tuple, ',')
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if fixed.len() < 2 {
            return Self::single(value.to_owned());
        }
        Self {
            fixed,
            variadic: None,
        }
    }

    pub(super) fn type_name(&self) -> String {
        if self.fixed.len() == 1 && self.variadic.is_none() {
            return self.fixed[0].clone();
        }
        let mut values = self.fixed.clone();
        if let Some(variadic) = &self.variadic {
            values.push(format!("...{variadic}"));
        }
        format!("({})", values.join(", "))
    }

    pub(super) fn value_at(&self, index: usize) -> String {
        self.fixed
            .get(index)
            .or(self.variadic.as_ref())
            .cloned()
            .unwrap_or_else(|| "nil".to_owned())
    }
}
