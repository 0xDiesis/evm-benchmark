/// Integration test for submission methods
///
/// This test demonstrates how to use both HTTP and WebSocket submission
/// methods with the dispatcher pattern.
#[cfg(test)]
mod tests {
    use evm_benchmark::config::SubmissionMethod;
    use evm_benchmark::submission::Submitter;
    use url::Url;

    #[test]
    fn test_http_submitter_creation() {
        let rpc_url = Url::parse("http://localhost:8545").expect("failed to parse rpc url");
        let ws_url = Url::parse("ws://localhost:8546").expect("failed to parse ws url");

        let submitter = Submitter::new(vec![rpc_url], &ws_url, 100, SubmissionMethod::Http);
        assert!(submitter.is_ok());
    }

    #[test]
    fn test_websocket_submitter_creation() {
        let rpc_url = Url::parse("http://localhost:8545").expect("failed to parse rpc url");
        let ws_url = Url::parse("ws://localhost:8546").expect("failed to parse ws url");

        let submitter = Submitter::new(vec![rpc_url], &ws_url, 100, SubmissionMethod::WebSocket);
        assert!(submitter.is_ok());
    }

    #[test]
    fn test_submission_method_dispatch() {
        let rpc_url = Url::parse("http://localhost:8545").expect("failed to parse rpc url");
        let ws_url = Url::parse("ws://localhost:8546").expect("failed to parse ws url");

        let http_result =
            Submitter::new(vec![rpc_url.clone()], &ws_url, 100, SubmissionMethod::Http);
        let ws_result = Submitter::new(vec![rpc_url], &ws_url, 100, SubmissionMethod::WebSocket);

        assert!(http_result.is_ok());
        assert!(ws_result.is_ok());
    }

    #[test]
    fn test_http_multi_endpoint() {
        let rpc_urls = vec![
            Url::parse("http://localhost:8545").unwrap(),
            Url::parse("http://localhost:8555").unwrap(),
            Url::parse("http://localhost:8565").unwrap(),
        ];
        let ws_url = Url::parse("ws://localhost:8546").expect("failed to parse ws url");

        let submitter = Submitter::new(rpc_urls, &ws_url, 100, SubmissionMethod::Http);
        assert!(submitter.is_ok());
    }
}
