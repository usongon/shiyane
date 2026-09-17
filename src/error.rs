use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Audio source error: {0}")]
    AudioSource(String),

    #[error("ASR error: {0}")]
    Asr(String),

    #[error("Translation error: {0}")]
    Translate(String),

    #[error("Configuration error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// 展开错误源链为「a → b → c」。reqwest 的 Display 只报
/// "error sending request for url (...)"，把 timeout/连接重置等底层分类全丢了，
/// 用户会被「网络问题」式模糊文案误导而无法排查。
fn error_causes(e: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = Vec::new();
    let mut cur = e.source();
    while let Some(err) = cur {
        parts.push(err.to_string());
        cur = err.source();
    }
    parts.join(" → ")
}

/// 传输层错误的统一装饰：补上分类（timeout/connect/transport）与源链，
/// 供 translate / OSS 等出网路径复用，杜绝「只报 error sending request」的盲区
pub fn transport_error(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else {
        "transport"
    };
    let causes = error_causes(e);
    if causes.is_empty() {
        format!("{e} [{kind}]")
    } else {
        format!("{e} [{kind}: {causes}]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[derive(Debug)]
    struct Leaf(&'static str);
    impl fmt::Display for Leaf {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }
    impl std::error::Error for Leaf {}

    #[derive(Debug)]
    struct Middle(&'static str, Leaf);
    impl fmt::Display for Middle {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }
    impl std::error::Error for Middle {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.1)
        }
    }

    #[test]
    fn error_causes_walks_full_chain() {
        let e = Middle("middle", Leaf("leaf"));
        // 顶层 Display 由调用方打印，这里只给源链（middle 之下的部分）
        assert_eq!(error_causes(&e), "leaf");
    }

    #[test]
    fn error_causes_empty_when_no_source() {
        let e = Leaf("alone");
        assert_eq!(error_causes(&e), "");
    }
}
