//! Normalization stage: canonicalizes SQL queries and HTTP paths.

/// Normalize a SQL query by replacing literal values with placeholders (stub).
pub fn normalize_sql(query: &str) -> String {
    // TODO: implement homemade SQL tokenizer
    query.to_string()
}

/// Normalize an HTTP path by replacing dynamic segments with placeholders (stub).
pub fn normalize_http(path: &str) -> String {
    // TODO: implement HTTP path normalization
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_stub_returns_input() {
        assert_eq!(normalize_sql("SELECT 1"), "SELECT 1");
    }

    #[test]
    fn http_stub_returns_input() {
        assert_eq!(normalize_http("/api/users/42"), "/api/users/42");
    }
}
