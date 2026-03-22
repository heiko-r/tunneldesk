use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRequest {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub tunnel_name: String,
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub raw_request: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredResponse {
    pub request_id: String,
    pub timestamp: DateTime<Utc>,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub raw_response: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestExchange {
    pub request: StoredRequest,
    pub response: Option<StoredResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredWebSocketMessage {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub tunnel_name: String,
    pub upgrade_request_id: String,
    pub direction: String, // "→" for outgoing, "←" for incoming
    pub message_type: WebSocketMessageType,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebSocketMessageType {
    Text,
    Binary,
}

#[derive(Debug, Clone, Default)]
pub struct WebSocketMessageFilter {
    pub tunnel_name: Option<String>,
    pub upgrade_request_id: Option<String>,
    pub direction: Option<String>,
    pub message_type: Option<WebSocketMessageType>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct QueryFilter {
    pub tunnel_name: Option<String>,
    pub method: Option<String>,
    pub status: Option<u16>,
    pub url_contains: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct WebSocketMessageStorage {
    messages: Arc<RwLock<HashMap<String, StoredWebSocketMessage>>>,
    max_messages: usize,
}

impl WebSocketMessageStorage {
    pub fn new(max_messages: usize) -> Self {
        Self {
            messages: Arc::new(RwLock::new(HashMap::new())),
            max_messages,
        }
    }

    pub async fn store_message(&self, message: StoredWebSocketMessage) {
        let mut messages = self.messages.write().await;

        // Remove oldest messages if we exceed the limit
        if messages.len() >= self.max_messages {
            let mut oldest_key = None;
            let mut oldest_timestamp = Utc::now();

            for (key, msg) in messages.iter() {
                if msg.timestamp < oldest_timestamp {
                    oldest_timestamp = msg.timestamp;
                    oldest_key = Some(key.clone());
                }
            }

            if let Some(key) = oldest_key {
                messages.remove(&key);
            }
        }

        messages.insert(message.id.clone(), message);
    }

    #[expect(dead_code)]
    pub async fn get_all_messages(&self) -> Vec<StoredWebSocketMessage> {
        let messages = self.messages.read().await;
        messages.values().cloned().collect()
    }

    pub async fn query_messages(
        &self,
        filter: &WebSocketMessageFilter,
    ) -> Vec<StoredWebSocketMessage> {
        let messages = self.messages.read().await;

        messages
            .values()
            .filter(|message| {
                // Filter by tunnel name
                if let Some(tunnel_name) = &filter.tunnel_name
                    && message.tunnel_name != *tunnel_name
                {
                    return false;
                }

                // Filter by upgrade request ID
                if let Some(upgrade_request_id) = &filter.upgrade_request_id
                    && message.upgrade_request_id != *upgrade_request_id
                {
                    return false;
                }

                // Filter by direction
                if let Some(direction) = &filter.direction
                    && message.direction != *direction
                {
                    return false;
                }

                // Filter by message type
                if let Some(filter_message_type) = &filter.message_type
                    && std::mem::discriminant(&message.message_type)
                        != std::mem::discriminant(filter_message_type)
                {
                    return false;
                }

                // Filter by timestamp range
                if let Some(since) = filter.since
                    && message.timestamp < since
                {
                    return false;
                }

                if let Some(until) = filter.until
                    && message.timestamp > until
                {
                    return false;
                }

                true
            })
            .cloned()
            .collect()
    }

    #[expect(dead_code)]
    pub async fn get_messages_by_upgrade_request_id(
        &self,
        upgrade_request_id: &str,
    ) -> Vec<StoredWebSocketMessage> {
        let filter = WebSocketMessageFilter {
            upgrade_request_id: Some(upgrade_request_id.to_string()),
            ..Default::default()
        };
        self.query_messages(&filter).await
    }

    #[expect(dead_code)]
    pub async fn get_message_by_id(&self, id: &str) -> Option<StoredWebSocketMessage> {
        let messages = self.messages.read().await;
        messages.get(id).cloned()
    }

    #[expect(dead_code)]
    pub async fn clear(&self) {
        let mut messages = self.messages.write().await;
        messages.clear();
    }

    #[expect(dead_code)]
    pub async fn get_count(&self) -> usize {
        let messages = self.messages.read().await;
        messages.len()
    }
}

#[derive(Clone, Debug)]
pub struct RequestStorage {
    requests: Arc<RwLock<HashMap<String, RequestExchange>>>,
    max_requests: usize,
}

impl RequestStorage {
    pub fn new(max_requests: usize) -> Self {
        Self {
            requests: Arc::new(RwLock::new(HashMap::new())),
            max_requests,
        }
    }

    pub async fn store_request(&self, request: StoredRequest) {
        let mut requests = self.requests.write().await;

        // Remove oldest requests if we exceed the limit
        if requests.len() >= self.max_requests {
            let mut oldest_key = None;
            let mut oldest_timestamp = Utc::now();

            for (key, exchange) in requests.iter() {
                if exchange.request.timestamp < oldest_timestamp {
                    oldest_timestamp = exchange.request.timestamp;
                    oldest_key = Some(key.clone());
                }
            }

            if let Some(key) = oldest_key {
                requests.remove(&key);
            }
        }

        let exchange = RequestExchange {
            request,
            response: None,
        };

        requests.insert(exchange.request.id.clone(), exchange);
    }

    pub async fn store_response(&self, response: StoredResponse) {
        let mut requests = self.requests.write().await;

        if let Some(exchange) = requests.get_mut(&response.request_id) {
            exchange.response = Some(response);
        }
    }

    #[expect(dead_code)]
    pub async fn get_all_requests(&self) -> Vec<RequestExchange> {
        let requests = self.requests.read().await;
        requests.values().cloned().collect()
    }

    #[expect(dead_code)]
    pub async fn query_requests(&self, filter: &QueryFilter) -> Vec<RequestExchange> {
        let requests = self.requests.read().await;

        requests
            .values()
            .filter(|exchange| {
                let request = &exchange.request;

                // Filter by tunnel name
                if let Some(tunnel_name) = &filter.tunnel_name
                    && request.tunnel_name != *tunnel_name
                {
                    return false;
                }

                // Filter by method
                if let Some(method) = &filter.method
                    && request.method.to_uppercase() != method.to_uppercase()
                {
                    return false;
                }

                // Filter by status (only if response exists)
                if let Some(status) = filter.status {
                    if let Some(response) = &exchange.response {
                        if response.status != status {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }

                // Filter by URL contains
                if let Some(url_contains) = &filter.url_contains
                    && !request.url.contains(url_contains)
                {
                    return false;
                }

                // Filter by timestamp range
                if let Some(since) = filter.since
                    && request.timestamp < since
                {
                    return false;
                }

                if let Some(until) = filter.until
                    && request.timestamp > until
                {
                    return false;
                }

                true
            })
            .cloned()
            .collect()
    }

    #[expect(dead_code)]
    pub async fn get_request_by_id(&self, id: &str) -> Option<RequestExchange> {
        let requests = self.requests.read().await;
        requests.get(id).cloned()
    }

    #[expect(dead_code)]
    pub async fn clear(&self) {
        let mut requests = self.requests.write().await;
        requests.clear();
    }

    #[expect(dead_code)]
    pub async fn get_count(&self) -> usize {
        let requests = self.requests.read().await;
        requests.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;

    fn create_test_request(id: &str, tunnel_name: &str, method: &str, url: &str) -> StoredRequest {
        StoredRequest {
            id: id.to_string(),
            timestamp: Utc::now(),
            tunnel_name: tunnel_name.to_string(),
            method: method.to_string(),
            url: url.to_string(),
            headers: HashMap::new(),
            body: vec![],
            raw_request: vec![],
        }
    }

    fn create_test_response(request_id: &str, status: u16) -> StoredResponse {
        StoredResponse {
            request_id: request_id.to_string(),
            timestamp: Utc::now(),
            status,
            headers: HashMap::new(),
            body: vec![],
            raw_response: vec![],
        }
    }

    #[tokio::test]
    async fn test_request_storage_new() {
        let storage = RequestStorage::new(100);
        assert_eq!(storage.get_count().await, 0);
    }

    #[tokio::test]
    async fn test_store_and_get_request() {
        let storage = RequestStorage::new(100);
        let request = create_test_request("req1", "tunnel1", "GET", "http://example.com");

        storage.store_request(request).await;
        assert_eq!(storage.get_count().await, 1);

        let retrieved = storage.get_request_by_id("req1").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().request.id, "req1");
    }

    #[tokio::test]
    async fn test_store_response() {
        let storage = RequestStorage::new(100);
        let request = create_test_request("req1", "tunnel1", "GET", "http://example.com");
        storage.store_request(request).await;

        let response = create_test_response("req1", 200);
        storage.store_response(response).await;

        let retrieved = storage.get_request_by_id("req1").await;
        assert!(retrieved.is_some());
        let exchange = retrieved.unwrap();
        assert!(exchange.response.is_some());
        assert_eq!(exchange.response.unwrap().status, 200);
    }

    #[tokio::test]
    async fn test_max_requests_limit() {
        let storage = RequestStorage::new(2);

        let req1 = create_test_request("req1", "tunnel1", "GET", "http://example1.com");
        let req2 = create_test_request("req2", "tunnel1", "POST", "http://example2.com");
        let req3 = create_test_request("req3", "tunnel1", "PUT", "http://example3.com");

        storage.store_request(req1).await;
        assert_eq!(storage.get_count().await, 1);

        storage.store_request(req2).await;
        assert_eq!(storage.get_count().await, 2);

        storage.store_request(req3).await;
        assert_eq!(storage.get_count().await, 2);

        // Oldest request should be removed
        assert!(storage.get_request_by_id("req1").await.is_none());
        assert!(storage.get_request_by_id("req2").await.is_some());
        assert!(storage.get_request_by_id("req3").await.is_some());
    }

    #[tokio::test]
    async fn test_query_requests_by_tunnel() {
        let storage = RequestStorage::new(100);

        let req1 = create_test_request("req1", "tunnel1", "GET", "http://example1.com");
        let req2 = create_test_request("req2", "tunnel2", "POST", "http://example2.com");
        let req3 = create_test_request("req3", "tunnel1", "PUT", "http://example3.com");

        storage.store_request(req1).await;
        storage.store_request(req2).await;
        storage.store_request(req3).await;

        let filter = QueryFilter {
            tunnel_name: Some("tunnel1".to_string()),
            ..Default::default()
        };

        let results = storage.query_requests(&filter).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.request.tunnel_name == "tunnel1"));
    }

    #[tokio::test]
    async fn test_query_requests_by_method() {
        let storage = RequestStorage::new(100);

        let req1 = create_test_request("req1", "tunnel1", "GET", "http://example1.com");
        let req2 = create_test_request("req2", "tunnel1", "POST", "http://example2.com");
        let req3 = create_test_request("req3", "tunnel1", "GET", "http://example3.com");

        storage.store_request(req1).await;
        storage.store_request(req2).await;
        storage.store_request(req3).await;

        let filter = QueryFilter {
            method: Some("GET".to_string()),
            ..Default::default()
        };

        let results = storage.query_requests(&filter).await;
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|r| r.request.method.to_uppercase() == "GET")
        );
    }

    #[tokio::test]
    async fn test_query_requests_by_status() {
        let storage = RequestStorage::new(100);

        let req1 = create_test_request("req1", "tunnel1", "GET", "http://example1.com");
        let req2 = create_test_request("req2", "tunnel1", "POST", "http://example2.com");

        storage.store_request(req1).await;
        storage.store_request(req2).await;

        let resp1 = create_test_response("req1", 200);
        let resp2 = create_test_response("req2", 404);

        storage.store_response(resp1).await;
        storage.store_response(resp2).await;

        let filter = QueryFilter {
            status: Some(200),
            ..Default::default()
        };

        let results = storage.query_requests(&filter).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].request.id, "req1");
    }

    #[tokio::test]
    async fn test_query_requests_by_url_contains() {
        let storage = RequestStorage::new(100);

        let req1 = create_test_request("req1", "tunnel1", "GET", "http://example.com/api/users");
        let req2 = create_test_request("req2", "tunnel1", "POST", "http://example.com/api/posts");
        let req3 = create_test_request("req3", "tunnel1", "GET", "http://other.com/data");

        storage.store_request(req1).await;
        storage.store_request(req2).await;
        storage.store_request(req3).await;

        let filter = QueryFilter {
            url_contains: Some("/api/".to_string()),
            ..Default::default()
        };

        let results = storage.query_requests(&filter).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.request.url.contains("/api/")));
    }

    #[tokio::test]
    async fn test_clear_storage() {
        let storage = RequestStorage::new(100);
        let request = create_test_request("req1", "tunnel1", "GET", "http://example.com");

        storage.store_request(request).await;
        assert_eq!(storage.get_count().await, 1);

        storage.clear().await;
        assert_eq!(storage.get_count().await, 0);
        assert!(storage.get_request_by_id("req1").await.is_none());
    }

    fn create_test_websocket_message(
        id: &str,
        tunnel_name: &str,
        upgrade_request_id: &str,
        direction: &str,
        message_type: WebSocketMessageType,
    ) -> StoredWebSocketMessage {
        StoredWebSocketMessage {
            id: id.to_string(),
            timestamp: Utc::now(),
            tunnel_name: tunnel_name.to_string(),
            upgrade_request_id: upgrade_request_id.to_string(),
            direction: direction.to_string(),
            message_type,
            payload: b"test payload".to_vec(),
        }
    }

    #[tokio::test]
    async fn test_websocket_storage_new() {
        let storage = WebSocketMessageStorage::new(100);
        assert_eq!(storage.get_count().await, 0);
    }

    #[tokio::test]
    async fn test_store_and_get_websocket_message() {
        let storage = WebSocketMessageStorage::new(100);
        let message = create_test_websocket_message(
            "msg1",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );

        storage.store_message(message).await;
        assert_eq!(storage.get_count().await, 1);

        let retrieved = storage.get_message_by_id("msg1").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, "msg1");
    }

    #[tokio::test]
    async fn test_websocket_max_messages_limit() {
        let storage = WebSocketMessageStorage::new(2);

        let msg1 = create_test_websocket_message(
            "msg1",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );
        let msg2 = create_test_websocket_message(
            "msg2",
            "tunnel1",
            "req1",
            "←",
            WebSocketMessageType::Binary,
        );
        let msg3 = create_test_websocket_message(
            "msg3",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );

        storage.store_message(msg1).await;
        assert_eq!(storage.get_count().await, 1);

        storage.store_message(msg2).await;
        assert_eq!(storage.get_count().await, 2);

        storage.store_message(msg3).await;
        assert_eq!(storage.get_count().await, 2);

        // Oldest message should be removed
        assert!(storage.get_message_by_id("msg1").await.is_none());
        assert!(storage.get_message_by_id("msg2").await.is_some());
        assert!(storage.get_message_by_id("msg3").await.is_some());
    }

    #[tokio::test]
    async fn test_query_websocket_messages_by_tunnel() {
        let storage = WebSocketMessageStorage::new(100);

        let msg1 = create_test_websocket_message(
            "msg1",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );
        let msg2 = create_test_websocket_message(
            "msg2",
            "tunnel2",
            "req1",
            "←",
            WebSocketMessageType::Binary,
        );
        let msg3 = create_test_websocket_message(
            "msg3",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );

        storage.store_message(msg1).await;
        storage.store_message(msg2).await;
        storage.store_message(msg3).await;

        let filter = WebSocketMessageFilter {
            tunnel_name: Some("tunnel1".to_string()),
            ..Default::default()
        };

        let results = storage.query_messages(&filter).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|m| m.tunnel_name == "tunnel1"));
    }

    #[tokio::test]
    async fn test_query_websocket_messages_by_direction() {
        let storage = WebSocketMessageStorage::new(100);

        let msg1 = create_test_websocket_message(
            "msg1",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );
        let msg2 = create_test_websocket_message(
            "msg2",
            "tunnel1",
            "req1",
            "←",
            WebSocketMessageType::Binary,
        );
        let msg3 = create_test_websocket_message(
            "msg3",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );

        storage.store_message(msg1).await;
        storage.store_message(msg2).await;
        storage.store_message(msg3).await;

        let filter = WebSocketMessageFilter {
            direction: Some("→".to_string()),
            ..Default::default()
        };

        let results = storage.query_messages(&filter).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|m| m.direction == "→"));
    }

    #[tokio::test]
    async fn test_query_websocket_messages_by_type() {
        let storage = WebSocketMessageStorage::new(100);

        let msg1 = create_test_websocket_message(
            "msg1",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );
        let msg2 = create_test_websocket_message(
            "msg2",
            "tunnel1",
            "req1",
            "←",
            WebSocketMessageType::Binary,
        );
        let msg3 = create_test_websocket_message(
            "msg3",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );

        storage.store_message(msg1).await;
        storage.store_message(msg2).await;
        storage.store_message(msg3).await;

        let filter = WebSocketMessageFilter {
            message_type: Some(WebSocketMessageType::Text),
            ..Default::default()
        };

        let results = storage.query_messages(&filter).await;
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|m| matches!(m.message_type, WebSocketMessageType::Text))
        );
    }

    #[tokio::test]
    async fn test_clear_websocket_storage() {
        let storage = WebSocketMessageStorage::new(100);
        let message = create_test_websocket_message(
            "msg1",
            "tunnel1",
            "req1",
            "→",
            WebSocketMessageType::Text,
        );

        storage.store_message(message).await;
        assert_eq!(storage.get_count().await, 1);

        storage.clear().await;
        assert_eq!(storage.get_count().await, 0);
        assert!(storage.get_message_by_id("msg1").await.is_none());
    }
}
