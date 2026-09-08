//! 搜索结果候选匹配（第 8.3 节）。
//!
//! 这个模块存在的唯一理由：**不许直接点第一个搜索结果**。
//! 站点的排序会变，同名书会有不同版本，搜「算法导论」可能先返回一本习题解答。
//! 点错的代价不是「下错一本」——文件会以正确书名写进 NAS，
//! 从此没人能发现那本书的内容是错的。
//!
//! 因此匹配按可信度从高到低分层，并且**宁可返回「待确认」也不猜**：
//!
//! 1. ISBN 标准化后完全一致 —— 站点自己给出的强标识，最可信；
//! 2. 书名标准化后完全一致，且作者也对得上；
//! 3. 书名轻微差异且作者、出版社归一化后均一致；
//! 4. 只有书名对得上而候选不唯一 —— 返回「待确认」，交给人或后续核验。
//!
//! 所有比较都在标准化之后进行：去掉空白、标点、大小写差异与全角字符，
//! 因为站点的标题里常出现「（第3版）」「[美]」这类噪声。

use crate::verify::normalize_title;

/// 一条搜索结果候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateBook {
    /// 在结果列表中的下标，选中后用于点击。
    pub index: usize,
    /// 卡片上的书名。
    pub title: String,
    /// 卡片上的作者，读不到时为空。
    pub author: String,
    /// 卡片上的出版社，读不到时为空。
    pub publisher: String,
    /// 卡片上的 ISBN，读不到时为空。
    pub isbn: String,
}

/// 选中候选的依据，写进执行日志便于回溯（第 8.3 节要求记录匹配依据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchBasis {
    /// ISBN 完全一致。
    Isbn,
    /// 书名一致且作者一致。
    TitleAndAuthor,
    /// 书名精确或轻微差异，作者和出版社均一致。
    Composite,
    /// 书名一致且候选唯一。
    UniqueTitle,
}

impl MatchBasis {
    /// 中文说明。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Isbn => "ISBN 完全一致",
            Self::TitleAndAuthor => "书名与作者一致",
            Self::Composite => "书名、作者、出版社综合匹配",
            Self::UniqueTitle => "书名一致且候选唯一",
        }
    }
}

/// 匹配结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchOutcome {
    /// 命中唯一候选。
    Matched {
        /// 选中的候选。
        candidate: CandidateBook,
        /// 匹配依据。
        basis: MatchBasis,
    },
    /// 有疑似候选但不能确定，需要人工或后续核验裁决。
    NeedsConfirm {
        /// 中文原因。
        reason: String,
    },
    /// 没有任何候选与目标书相符。
    NotFound {
        /// 中文原因。
        reason: String,
    },
}

/// 使用领域层的校验位检查，将有效 ISBN-10/13 统一为 ISBN-13。
pub fn normalize_isbn(raw: &str) -> String {
    platform_domain::isbn::normalize_isbn(raw)
        .map(|isbn| isbn.to_string())
        .unwrap_or_default()
}

/// 标题轻微差异：至少 8 字符、编辑距离不超过 10%，卷号/版本数字必须一致。
fn similar_title(left: &str, right: &str) -> bool {
    let a: Vec<char> = normalize_title(left).chars().collect();
    let b: Vec<char> = normalize_title(right).chars().collect();
    let max_len = a.len().max(b.len());
    if a == b {
        return !a.is_empty();
    }
    if a.len().min(b.len()) < 8 || max_len > 1000 || a.len().abs_diff(b.len()) > max_len / 10 {
        return false;
    }
    if a.iter().filter(|c| c.is_numeric()).collect::<Vec<_>>()
        != b.iter().filter(|c| c.is_numeric()).collect::<Vec<_>>()
    {
        return false;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(above + 1)
                .min(diagonal + usize::from(ca != cb));
            diagonal = above;
        }
    }
    row[b.len()] <= max_len / 10
}

/// 标准化人名/机构名：去空白与常见标点，转小写，剥掉国别方括号。
pub fn normalize_person(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_bracket = false;
    for ch in raw.chars() {
        match ch {
            '[' | '［' | '（' | '(' => in_bracket = true,
            ']' | '］' | '）' | ')' => in_bracket = false,
            _ if in_bracket => {}
            c if c.is_alphanumeric() => {
                for lowered in c.to_lowercase() {
                    out.push(lowered);
                }
            }
            _ => {}
        }
    }
    out
}

/// 匹配过程的可追溯记录（第 8.3 节要求记录搜索词、候选数量、选中候选与匹配依据）。
///
/// 这份记录跟着执行结果一起写进日志。事后发现「书对不上」时，
/// 它是唯一能回答「当时站点返回了什么、我们凭什么选它」的东西。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MatchRecord {
    /// 实际提交给站点的搜索词。
    pub search_term: String,
    /// 站点返回的候选数量。
    pub candidate_count: usize,
    /// 选中候选的书名。
    pub chosen_title: String,
    /// 选中候选的作者。
    pub chosen_author: String,
    /// 选中候选的 ISBN。
    pub chosen_isbn: String,
    /// 匹配依据（中文）。
    pub basis: String,
}

impl MatchRecord {
    /// 由搜索词、候选总数与命中结果组装。
    pub fn new(
        search_term: impl Into<String>,
        candidate_count: usize,
        candidate: &CandidateBook,
        basis: MatchBasis,
    ) -> Self {
        Self {
            search_term: search_term.into(),
            candidate_count,
            chosen_title: candidate.title.clone(),
            chosen_author: candidate.author.clone(),
            chosen_isbn: candidate.isbn.clone(),
            basis: basis.as_str().to_string(),
        }
    }

    /// 一行中文摘要，用于执行日志。
    pub fn summary(&self) -> String {
        format!(
            "搜索词「{}」返回 {} 个候选，选中《{}》（作者：{}，ISBN：{}），依据：{}",
            self.search_term,
            self.candidate_count,
            self.chosen_title,
            if self.chosen_author.is_empty() {
                "未知"
            } else {
                &self.chosen_author
            },
            if self.chosen_isbn.is_empty() {
                "未知"
            } else {
                &self.chosen_isbn
            },
            self.basis
        )
    }
}

/// 目标图书的匹配条件。
#[derive(Debug, Clone)]
pub struct MatchTarget<'a> {
    /// 目标书名。
    pub title: &'a str,
    /// 目标作者。
    pub author: Option<&'a str>,
    /// 目标出版社。
    pub publisher: Option<&'a str>,
    /// 目标 ISBN。
    pub isbn: Option<&'a str>,
}

/// 在候选列表中挑出唯一确定的一本（第 8.3 节的四层顺序）。
pub fn select_candidate(target: &MatchTarget<'_>, candidates: &[CandidateBook]) -> MatchOutcome {
    let isbn = target.isbn.map(normalize_isbn).unwrap_or_default();
    if !isbn.is_empty() {
        let hits: Vec<_> = candidates
            .iter()
            .filter(|c| normalize_isbn(&c.isbn) == isbn)
            .collect();
        if hits.len() == 1 {
            return MatchOutcome::Matched {
                candidate: hits[0].clone(),
                basis: MatchBasis::Isbn,
            };
        }
        if hits.len() > 1 {
            return MatchOutcome::NeedsConfirm {
                reason: "多个可下载候选 ISBN 等价，无法确定唯一结果".to_string(),
            };
        }
    }
    let title = normalize_title(target.title);
    let author = target.author.map(normalize_person).unwrap_or_default();
    let publisher = target.publisher.map(normalize_person).unwrap_or_default();
    let mut hits = Vec::new();
    let mut title_seen = false;
    for c in candidates {
        let same_author = !author.is_empty() && normalize_person(&c.author) == author;
        let same_publisher = !publisher.is_empty() && normalize_person(&c.publisher) == publisher;
        let exact = !title.is_empty() && normalize_title(&c.title) == title;
        let similar = same_author && same_publisher && similar_title(&c.title, target.title);
        // 仅书名任务保留原有版本后缀兼容；有元数据时由作者/出版社约束。
        let title_only = author.is_empty()
            && publisher.is_empty()
            && crate::verify::filename_matches_title(std::path::Path::new(&c.title), target.title);
        if !(exact || similar || title_only) {
            continue;
        }
        title_seen = true;
        if (!author.is_empty() && !same_author) || (!publisher.is_empty() && !same_publisher) {
            continue;
        }
        let basis = if same_author && same_publisher {
            MatchBasis::Composite
        } else if same_author {
            MatchBasis::TitleAndAuthor
        } else {
            MatchBasis::UniqueTitle
        };
        hits.push((c, basis, exact));
    }
    if hits.iter().any(|(_, _, exact)| *exact) {
        hits.retain(|(_, _, exact)| *exact);
    }
    if hits.len() == 1 {
        return MatchOutcome::Matched {
            candidate: hits[0].0.clone(),
            basis: hits[0].1,
        };
    }
    if title_seen {
        MatchOutcome::NeedsConfirm {
            reason: format!(
                "候选书作者/出版社不一致、缺失或存在多个匹配（{} 个），不自动下载",
                hits.len()
            ),
        }
    } else {
        MatchOutcome::NotFound {
            reason: format!(
                "{} 个候选中没有与《{}》匹配的图书",
                candidates.len(),
                target.title
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        index: usize,
        title: &str,
        author: &str,
        publisher: &str,
        isbn: &str,
    ) -> CandidateBook {
        CandidateBook {
            index,
            title: title.to_string(),
            author: author.to_string(),
            publisher: publisher.to_string(),
            isbn: isbn.to_string(),
        }
    }

    fn target<'a>(
        title: &'a str,
        author: Option<&'a str>,
        publisher: Option<&'a str>,
        isbn: Option<&'a str>,
    ) -> MatchTarget<'a> {
        MatchTarget {
            title,
            author,
            publisher,
            isbn,
        }
    }

    #[test]
    fn valid_isbn10_and_13_are_equivalent_but_bad_checksums_are_not() {
        for (ten, thirteen) in [
            ("0262033844", "9780262033848"),
            ("043942089X", "9780439420891"),
        ] {
            for (wanted, offered) in [(ten, thirteen), (thirteen, ten)] {
                let result = select_candidate(
                    &target("Wanted", None, None, Some(wanted)),
                    &[candidate(0, "Different title", "", "", offered)],
                );
                assert!(matches!(
                    result,
                    MatchOutcome::Matched {
                        basis: MatchBasis::Isbn,
                        ..
                    }
                ));
            }
        }
        assert_eq!(normalize_isbn("9787111407011"), "");
        assert!(!matches!(
            select_candidate(
                &target("Wanted", None, None, Some("9787111407011")),
                &[candidate(0, "Wrong book", "", "", "9787111407011")]
            ),
            MatchOutcome::Matched { .. }
        ));
    }

    #[test]
    fn slightly_different_title_requires_both_metadata_and_unique_candidate() {
        let t = target(
            "Digital Twins for Cities",
            Some("Alice Smith"),
            Some("Example Press"),
            None,
        );
        let book = candidate(
            0,
            "Digital Twin for Cities",
            "Alice Smith",
            "Example Press",
            "",
        );
        assert!(matches!(
            select_candidate(&t, std::slice::from_ref(&book)),
            MatchOutcome::Matched {
                basis: MatchBasis::Composite,
                ..
            }
        ));
        for (author, publisher) in [
            ("", "Example Press"),
            ("Alice Smith", ""),
            ("Bob", "Example Press"),
            ("Alice Smith", "Other Press"),
        ] {
            assert!(!matches!(
                select_candidate(&t, &[candidate(0, &book.title, author, publisher, "")]),
                MatchOutcome::Matched { .. }
            ));
        }
        assert!(matches!(
            select_candidate(
                &t,
                &[
                    book.clone(),
                    candidate(1, &book.title, &book.author, &book.publisher, "")
                ]
            ),
            MatchOutcome::NeedsConfirm { .. }
        ));
        assert!(!similar_title(
            "Digital Twins Volume 1",
            "Digital Twins Volume 2"
        ));
        assert!(!similar_title(
            "Digital Twins for Cities",
            "Cooking for Beginners"
        ));
    }

    #[test]
    fn exact_title_does_not_override_conflicting_publisher() {
        let t = target("Digital Twins", Some("Alice"), Some("Publisher A"), None);
        assert!(matches!(
            select_candidate(
                &t,
                &[candidate(0, "Digital Twins", "Alice", "Publisher B", "")]
            ),
            MatchOutcome::NeedsConfirm { .. }
        ));
    }

    #[test]
    fn isbn_wins_regardless_of_position() {
        // 正确的书排在第三位：直接点第一个就会下错
        let candidates = vec![
            candidate(
                0,
                "算法导论习题解答",
                "无",
                "机械工业出版社",
                "9780000000000",
            ),
            candidate(
                1,
                "算法导论 第2版",
                "科曼",
                "机械工业出版社",
                "9781111111111",
            ),
            candidate(
                2,
                "算法导论",
                "Thomas H. Cormen",
                "机械工业出版社",
                "978-7-111-40701-0",
            ),
        ];
        let t = target("算法导论", None, None, Some("9787111407010"));
        match select_candidate(&t, &candidates) {
            MatchOutcome::Matched { candidate, basis } => {
                assert_eq!(candidate.index, 2);
                assert_eq!(basis, MatchBasis::Isbn);
            }
            other => panic!("应按 ISBN 命中，实际 {other:?}"),
        }
    }

    #[test]
    fn title_and_author_disambiguate_same_title() {
        let candidates = vec![
            candidate(0, "算法导论", "别的作者", "某出版社", ""),
            candidate(1, "算法导论", "[美] Cormen", "机械工业出版社", ""),
        ];
        let t = target("算法导论", Some("Cormen"), None, None);
        match select_candidate(&t, &candidates) {
            MatchOutcome::Matched { candidate, basis } => {
                assert_eq!(candidate.index, 1);
                assert_eq!(basis, MatchBasis::TitleAndAuthor);
            }
            other => panic!("应按书名+作者命中，实际 {other:?}"),
        }
    }

    #[test]
    fn publisher_breaks_the_remaining_tie() {
        let candidates = vec![
            candidate(0, "算法导论", "Cormen", "人民邮电出版社", ""),
            candidate(1, "算法导论", "Cormen", "机械工业出版社", ""),
        ];
        let t = target("算法导论", Some("Cormen"), Some("机械工业出版社"), None);
        match select_candidate(&t, &candidates) {
            MatchOutcome::Matched { candidate, basis } => {
                assert_eq!(candidate.index, 1);
                assert_eq!(basis, MatchBasis::Composite);
            }
            other => panic!("应按综合匹配命中，实际 {other:?}"),
        }
    }

    #[test]
    fn title_only_with_multiple_candidates_needs_confirm() {
        // 第 8.3 节第 4 条：只有书名且存在多个候选时不得猜测
        let candidates = vec![
            candidate(0, "算法导论", "", "", ""),
            candidate(1, "算法导论", "", "", ""),
        ];
        let t = target("算法导论", None, None, None);
        assert!(matches!(
            select_candidate(&t, &candidates),
            MatchOutcome::NeedsConfirm { .. }
        ));
    }

    #[test]
    fn title_only_with_single_candidate_is_matched() {
        let candidates = vec![
            candidate(0, "算法导论习题解答", "", "", ""),
            candidate(1, "算法导论", "", "", ""),
        ];
        let t = target("算法导论", None, None, None);
        match select_candidate(&t, &candidates) {
            MatchOutcome::Matched { candidate, basis } => {
                assert_eq!(candidate.index, 1);
                assert_eq!(basis, MatchBasis::UniqueTitle);
            }
            other => panic!("唯一书名候选应命中，实际 {other:?}"),
        }
    }

    #[test]
    fn no_title_match_is_not_found() {
        let candidates = vec![candidate(0, "深入理解计算机系统", "", "", "")];
        let t = target("算法导论", None, None, None);
        assert!(matches!(
            select_candidate(&t, &candidates),
            MatchOutcome::NotFound { .. }
        ));
    }

    #[test]
    fn empty_result_is_not_found() {
        let t = target("算法导论", None, None, None);
        assert!(matches!(
            select_candidate(&t, &[]),
            MatchOutcome::NotFound { .. }
        ));
    }

    #[test]
    fn wrong_author_is_never_silently_downloaded() {
        // 书名对、作者全不对：这是最危险的情形，绝不能直接下载
        let candidates = vec![candidate(0, "算法导论", "张三", "某出版社", "")];
        let t = target("算法导论", Some("Cormen"), None, None);
        assert!(matches!(
            select_candidate(&t, &candidates),
            MatchOutcome::NeedsConfirm { .. }
        ));
    }

    #[test]
    fn isbn_normalization_ignores_separators() {
        assert_eq!(normalize_isbn("978-7-111-40701-0"), "9787111407010");
        assert_eq!(normalize_isbn("0-262-03384-4"), "9780262033848");
    }

    #[test]
    fn person_normalization_strips_country_marks() {
        assert_eq!(normalize_person("[美] Thomas H. Cormen"), "thomashcormen");
        assert_eq!(normalize_person("（美）科曼"), "科曼");
    }

    #[test]
    fn edition_suffix_is_accepted_when_no_exact_title() {
        let candidates = vec![candidate(0, "水利工程建设投资控制 第2版", "", "", "")];
        let t = target("水利工程建设投资控制", None, None, None);
        match select_candidate(&t, &candidates) {
            MatchOutcome::Matched { candidate, .. } => {
                assert_eq!(candidate.index, 0);
            }
            other => panic!("版本后缀应命中，实际 {other:?}"),
        }
    }
}
