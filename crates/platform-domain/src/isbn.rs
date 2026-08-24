//! ISBN 规范化与校验（第 8.2 节去重规则的强唯一键）。
//!
//! 只有通过校验位验证的 ISBN 才被当作强唯一键；否则退化为书名+作者+出版社匹配，
//! 避免脏 ISBN（例如 CSV 里填成条码或订货号）把不同的书合并成一本。

/// 规范化后的 ISBN，始终为 13 位数字。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Isbn(String);

impl Isbn {
    /// 13 位数字形态。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Isbn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 规范化 ISBN：去掉连字符与空格，校验位合法时统一转为 ISBN-13。
///
/// 返回 `None` 表示输入不是合法 ISBN，调用方应退化到书名匹配。
pub fn normalize_isbn(raw: &str) -> Option<Isbn> {
    let digits: String = raw
        .chars()
        .filter(|c| !matches!(c, '-' | ' ' | '\u{3000}' | '_' | '.'))
        .collect();
    let upper = digits.to_ascii_uppercase();

    match upper.len() {
        10 if is_valid_isbn10(&upper) => Some(Isbn(isbn10_to_13(&upper))),
        13 if is_valid_isbn13(&upper) => Some(Isbn(upper)),
        _ => None,
    }
}

fn is_valid_isbn10(value: &str) -> bool {
    let mut sum = 0u32;
    for (index, ch) in value.chars().enumerate() {
        let digit = match ch {
            '0'..='9' => ch as u32 - '0' as u32,
            // 末位允许校验字符 X（代表 10）
            'X' if index == 9 => 10,
            _ => return false,
        };
        sum += digit * (10 - index as u32);
    }
    sum.is_multiple_of(11)
}

fn is_valid_isbn13(value: &str) -> bool {
    if !value.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let sum: u32 = value
        .chars()
        .enumerate()
        .map(|(index, ch)| {
            let digit = ch as u32 - '0' as u32;
            if index % 2 == 0 {
                digit
            } else {
                digit * 3
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

fn isbn10_to_13(value: &str) -> String {
    let core: String = format!("978{}", &value[..9]);
    let sum: u32 = core
        .chars()
        .enumerate()
        .map(|(index, ch)| {
            let digit = ch as u32 - '0' as u32;
            if index % 2 == 0 {
                digit
            } else {
                digit * 3
            }
        })
        .sum();
    let check = (10 - (sum % 10)) % 10;
    format!("{core}{check}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_isbn13_with_separators() {
        let isbn = normalize_isbn("978-7-111-40701-0").unwrap();
        assert_eq!(isbn.as_str(), "9787111407010");
    }

    #[test]
    fn upgrades_isbn10_to_isbn13() {
        // 《算法导论》ISBN-10 0262033844 → ISBN-13 9780262033848
        let isbn = normalize_isbn("0-262-03384-4").unwrap();
        assert_eq!(isbn.as_str(), "9780262033848");
    }

    #[test]
    fn accepts_isbn10_with_x_check_digit() {
        assert_eq!(
            normalize_isbn("043942089X").unwrap().as_str(),
            normalize_isbn("9780439420891").unwrap().as_str()
        );
    }

    #[test]
    fn rejects_bad_check_digit_and_garbage() {
        assert!(normalize_isbn("9787111407011").is_none());
        assert!(normalize_isbn("12345").is_none());
        assert!(normalize_isbn("").is_none());
        assert!(normalize_isbn("订货号ABCDEFGHIJ").is_none());
    }
}
