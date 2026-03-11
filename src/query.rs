use glob::Pattern;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryPlan {
    pub include_terms: Vec<String>,
    pub exclude_terms: Vec<String>,
    pub ext_filters: Vec<String>,
    pub path_globs: Vec<String>,
}

impl QueryPlan {
    pub fn is_empty(&self) -> bool {
        self.include_terms.is_empty()
            && self.exclude_terms.is_empty()
            && self.ext_filters.is_empty()
            && self.path_globs.is_empty()
    }
}

pub fn parse_query(input: &str) -> QueryPlan {
    let mut plan = QueryPlan::default();

    for raw_token in input.split_whitespace() {
        let token = raw_token.trim();
        if token.is_empty() {
            continue;
        }

        if let Some(negated) = token.strip_prefix('!') {
            if !negated.is_empty() {
                plan.exclude_terms.push(negated.to_lowercase());
            }
            continue;
        }

        if let Some(ext) = token.strip_prefix("ext:") {
            if !ext.is_empty() {
                plan.ext_filters
                    .push(normalize_extension_suffix(ext.to_lowercase().as_str()));
            }
            continue;
        }

        if is_extension_alias(token) {
            plan.ext_filters.push(token.to_lowercase());
            continue;
        }

        if is_path_glob(token) {
            plan.path_globs.push(token.to_lowercase());
            continue;
        }

        plan.include_terms.push(token.to_lowercase());
    }

    plan
}

pub fn path_matches_query(path: &str, plan: &QueryPlan) -> bool {
    let normalized = normalize_path(path);
    let basename = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    if !plan.ext_filters.is_empty()
        && !plan
            .ext_filters
            .iter()
            .any(|suffix| extension_filter_matches(suffix, &normalized, &basename))
    {
        return false;
    }

    for glob in &plan.path_globs {
        if !glob_matches_path(glob, &normalized) {
            return false;
        }
    }

    for term in &plan.include_terms {
        if !normalized.contains(term) {
            return false;
        }
    }

    for term in &plan.exclude_terms {
        if normalized.contains(term) || basename.contains(term) {
            return false;
        }
    }

    true
}

fn extension_filter_matches(suffix: &str, normalized_path: &str, basename: &str) -> bool {
    if has_glob_meta(suffix) {
        let pattern_candidate = if suffix.starts_with('*') {
            suffix.to_string()
        } else {
            format!("*{}", suffix)
        };

        if let Ok(pattern) = Pattern::new(&pattern_candidate) {
            return pattern.matches(basename);
        }
        false
    } else {
        normalized_path.ends_with(suffix)
    }
}

pub fn score_path(path: &str, plan: &QueryPlan) -> i32 {
    let normalized = normalize_path(path);
    let basename = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mut score = 0;

    for term in &plan.include_terms {
        if basename == *term {
            score += 120;
        } else if basename.starts_with(term) {
            score += 80;
        } else if basename.contains(term) {
            score += 50;
        } else if normalized.contains(term) {
            score += 20;
        }
    }

    score += (plan.ext_filters.len() as i32) * 20;
    score += (plan.path_globs.len() as i32) * 15;
    score - normalized.len() as i32
}

fn normalize_extension_suffix(ext: &str) -> String {
    let trimmed = ext.trim_start_matches('.');
    format!(".{}", trimmed)
}

fn is_extension_alias(token: &str) -> bool {
    token.starts_with('.') && !token[1..].is_empty() && !token.contains('/')
}

fn is_path_glob(token: &str) -> bool {
    token.starts_with('/')
        || token.contains('/')
        || token.contains('*')
        || token.contains('?')
        || token.contains('[')
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

fn glob_matches_path(pattern: &str, path: &str) -> bool {
    let normalized = pattern.trim().replace('\\', "/").to_lowercase();
    let trimmed = normalized.trim_start_matches('/').trim_end_matches('/');

    if trimmed.is_empty() {
        return true;
    }

    let candidates = if has_glob_meta(trimmed) {
        vec![
            normalized.clone(),
            trimmed.to_string(),
            format!("**/{}", trimmed),
        ]
    } else {
        vec![
            // Intuitive directory scope token (e.g. "/src") should match
            // anywhere in the path tree.
            format!("**/{}/**", trimmed),
            format!("**/{}", trimmed),
        ]
    };

    for candidate in candidates {
        if let Ok(glob) = Pattern::new(&candidate) {
            if glob.matches(path) {
                return true;
            }
        }
    }

    false
}

fn has_glob_meta(token: &str) -> bool {
    token.contains('*') || token.contains('?') || token.contains('[')
}

#[cfg(test)]
mod tests {
    use super::{parse_query, path_matches_query};

    #[test]
    fn parse_ext_aliases() {
        let dot = parse_query(".rs");
        let ext = parse_query("ext:rs");
        assert_eq!(dot.ext_filters, ext.ext_filters);
        assert_eq!(dot.ext_filters, vec![".rs"]);
    }

    #[test]
    fn parse_path_glob() {
        let plan = parse_query("/src/*.tsx");
        assert_eq!(plan.path_globs, vec!["/src/*.tsx"]);
    }

    #[test]
    fn parse_negation() {
        let plan = parse_query("!test");
        assert_eq!(plan.exclude_terms, vec!["test"]);
    }

    #[test]
    fn parse_mixed_query() {
        let plan = parse_query("ext:rs !test /src/*.rs parser");
        assert_eq!(plan.ext_filters, vec![".rs"]);
        assert_eq!(plan.exclude_terms, vec!["test"]);
        assert_eq!(plan.path_globs, vec!["/src/*.rs"]);
        assert_eq!(plan.include_terms, vec!["parser"]);
    }

    #[test]
    fn path_scope_without_wildcard_matches_subdirectories() {
        let plan = parse_query(".tsx /src");
        assert!(path_matches_query("/repo/src/ui/app.tsx", &plan));
        assert!(!path_matches_query("/repo/tests/app.tsx", &plan));
    }
}
