use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

/// A captured HTTP request, stored in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRequest {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// When the request was received.
    pub timestamp: DateTime<Utc>,
    /// Name of the tunnel this request arrived on.
    pub tunnel_name: String,
    /// HTTP method (e.g. `"GET"`).
    pub method: String,
    /// Request path and query string (e.g. `"/api/users?page=1"`).
    pub url: String,
    /// Parsed HTTP headers.
    pub headers: HashMap<String, String>,
    /// Body bytes, potentially truncated to the configured `max_request_body_size`.
    pub body: Vec<u8>,
    /// Complete raw request bytes as received, potentially truncated.
    pub raw_request: Vec<u8>,
}

/// A captured HTTP response, stored in memory and linked to a [`StoredRequest`] by ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredResponse {
    /// ID of the [`StoredRequest`] this response corresponds to.
    pub request_id: String,
    /// When the response was received.
    pub timestamp: DateTime<Utc>,
    /// HTTP status code.
    pub status: u16,
    /// Parsed HTTP headers.
    pub headers: HashMap<String, String>,
    /// Body bytes, potentially truncated to the configured `max_request_body_size`.
    pub body: Vec<u8>,
    /// Complete raw response bytes as received, potentially truncated.
    pub raw_response: Vec<u8>,
    /// Round-trip time from when the request was stored until this response was
    /// received, in milliseconds.  `None` when the request could not be matched.
    pub response_time_ms: Option<f64>,
}

/// A paired HTTP request and its optional response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestExchange {
    pub request: StoredRequest,
    /// `None` until the corresponding response has been captured.
    pub response: Option<StoredResponse>,
}

/// A single WebSocket frame captured from a proxied connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredWebSocketMessage {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// When the frame was captured.
    pub timestamp: DateTime<Utc>,
    /// Name of the tunnel this message was observed on.
    pub tunnel_name: String,
    /// ID of the HTTP upgrade request that opened this WebSocket connection.
    pub upgrade_request_id: String,
    /// Traffic direction: `"→"` for client→server, `"←"` for server→client.
    pub direction: String,
    pub message_type: WebSocketMessageType,
    /// Unmasked payload bytes.
    pub payload: Vec<u8>,
}

/// WebSocket frame payload encoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebSocketMessageType {
    Text,
    Binary,
}

/// Filter criteria for [`WebSocketMessageStorage::query_messages`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebSocketMessageFilter {
    pub tunnel_name: Option<String>,
    pub upgrade_request_id: Option<String>,
    /// Match on direction string (e.g. `"→"` or `"←"`).
    pub direction: Option<String>,
    pub message_type: Option<WebSocketMessageType>,
    /// Inclusive lower bound on message timestamp.
    pub since: Option<DateTime<Utc>>,
    /// Inclusive upper bound on message timestamp.
    pub until: Option<DateTime<Utc>>,
}

/// Sort order for [`RequestStorage::query_requests`] results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Filter and sort criteria for [`RequestStorage::query_requests`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryFilter {
    pub tunnel_name: Option<String>,
    /// Case-insensitive HTTP method match (e.g. `"get"` matches `"GET"`).
    pub method: Option<String>,
    pub status: Option<u16>,
    /// Substring match against the request URL.
    pub url_contains: Option<String>,
    /// Inclusive lower bound on request timestamp.
    pub since: Option<DateTime<Utc>>,
    /// Inclusive upper bound on request timestamp.
    pub until: Option<DateTime<Utc>>,
    /// Result sort order; defaults to [`SortDirection::Desc`] (newest first).
    pub sort_direction: Option<SortDirection>,
}

impl QueryFilter {
    /// Returns `true` when `exchange` satisfies all criteria in this filter.
    pub fn matches(&self, exchange: &RequestExchange) -> bool {
        let request = &exchange.request;

        if let Some(tunnel_name) = &self.tunnel_name
            && request.tunnel_name != *tunnel_name
        {
            return false;
        }

        if let Some(method) = &self.method
            && request.method.to_uppercase() != method.to_uppercase()
        {
            return false;
        }

        if let Some(status) = self.status {
            if let Some(response) = &exchange.response {
                if response.status != status {
                    return false;
                }
            } else {
                return false;
            }
        }

        if let Some(url_contains) = &self.url_contains
            && !request.url.contains(url_contains.as_str())
        {
            return false;
        }

        if let Some(since) = self.since
            && request.timestamp < since
        {
            return false;
        }

        if let Some(until) = self.until
            && request.timestamp > until
        {
            return false;
        }

        true
    }
}

/// Thread-safe in-memory store for captured WebSocket frames.
#[derive(Clone, Debug)]
pub struct WebSocketMessageStorage {
    messages: Arc<RwLock<HashMap<String, StoredWebSocketMessage>>>,
    max_messages: usize,
    message_sender: broadcast::Sender<StoredWebSocketMessage>,
}

impl WebSocketMessageStorage {
    /// Creates a new storage that retains at most `max_messages` frames.
    pub fn new(max_messages: usize) -> Self {
        let (message_sender, _) = broadcast::channel(1000);
        Self {
            messages: Arc::new(RwLock::new(HashMap::new())),
            max_messages,
            message_sender,
        }
    }

    /// Stores `message`, evicting the oldest entry when the capacity is full.
    /// Broadcasts the stored message to all active subscribers.
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

        messages.insert(message.id.clone(), message.clone());

        // Broadcast the new message
        let _ = self.message_sender.send(message);
    }

    #[cfg(test)]
    pub async fn get_all_messages(&self) -> Vec<StoredWebSocketMessage> {
        let messages = self.messages.read().await;
        messages.values().cloned().collect()
    }

    /// Returns all stored messages that satisfy `filter`, in arbitrary order.
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

    #[cfg(test)]
    pub async fn get_message_by_id(&self, id: &str) -> Option<StoredWebSocketMessage> {
        let messages = self.messages.read().await;
        messages.get(id).cloned()
    }

    #[cfg(test)]
    pub async fn get_count(&self) -> usize {
        let messages = self.messages.read().await;
        messages.len()
    }

    /// Returns a broadcast receiver that delivers each newly stored message.
    pub fn subscribe_messages(&self) -> broadcast::Receiver<StoredWebSocketMessage> {
        self.message_sender.subscribe()
    }
}

/// Thread-safe in-memory store for captured HTTP request–response exchanges.
#[derive(Clone, Debug)]
pub struct RequestStorage {
    requests: Arc<RwLock<HashMap<String, RequestExchange>>>,
    pending_requests_per_connection: Arc<RwLock<HashMap<String, VecDeque<String>>>>,
    max_requests: usize,
    request_sender: broadcast::Sender<RequestExchange>,
}

impl RequestStorage {
    /// Creates a new storage that retains at most `max_requests` exchanges.
    pub fn new(max_requests: usize) -> Self {
        let (request_sender, _) = broadcast::channel(1000);
        Self {
            requests: Arc::new(RwLock::new(HashMap::new())),
            pending_requests_per_connection: Arc::new(RwLock::new(HashMap::new())),
            max_requests,
            request_sender,
        }
    }

    /// Stores `request` as a new exchange, evicting the oldest entry when at capacity.
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
                // FIXME: Also delete websocket messages?
            }
        }

        let exchange = RequestExchange {
            request: request.clone(),
            response: None,
        };

        requests.insert(request.id.clone(), exchange);
    }

    /// Stores `request` and enqueues its ID in `connection_id`'s pending queue so
    /// that the next response on that connection can be matched to it (FIFO).
    pub async fn store_request_with_connection(&self, request: StoredRequest, connection_id: &str) {
        // Store the request normally
        self.store_request(request.clone()).await;

        // Add to connection's pending queue
        let mut pending = self.pending_requests_per_connection.write().await;
        pending
            .entry(connection_id.to_string())
            .or_insert_with(VecDeque::new)
            .push_back(request.id.clone());
    }

    /// Dequeues and returns the `(id, timestamp)` of the oldest unmatched request
    /// for `connection_id`.  Returns `None` when no pending request exists.
    pub async fn get_next_pending_request_for_connection(
        &self,
        connection_id: &str,
    ) -> Option<(String, DateTime<Utc>)> {
        let request_id = {
            let mut pending = self.pending_requests_per_connection.write().await;
            pending.get_mut(connection_id)?.pop_front()?
        };
        let requests = self.requests.read().await;
        let timestamp = requests.get(&request_id)?.request.timestamp;
        Some((request_id, timestamp))
    }

    /// Attaches `response` to its corresponding request exchange and broadcasts
    /// the completed exchange to all active subscribers.  Does nothing when no
    /// exchange with `response.request_id` exists.
    pub async fn store_response(&self, response: StoredResponse) {
        let mut requests = self.requests.write().await;

        if let Some(exchange) = requests.get_mut(&response.request_id) {
            exchange.response = Some(response.clone());

            // Broadcast the complete exchange when response is stored
            let _ = self.request_sender.send(exchange.clone());
        }
    }

    #[cfg(test)]
    pub async fn get_all_requests(&self) -> Vec<RequestExchange> {
        let requests = self.requests.read().await;
        requests.values().cloned().collect()
    }

    /// Returns all exchanges that satisfy `filter`, sorted according to
    /// `filter.sort_direction` (default: newest first).
    pub async fn query_requests(&self, filter: &QueryFilter) -> Vec<RequestExchange> {
        let requests = self.requests.read().await;

        let mut results: Vec<RequestExchange> = requests
            .values()
            .filter(|exchange| filter.matches(exchange))
            .cloned()
            .collect();

        // Sort by timestamp based on sort_direction
        match filter.sort_direction {
            Some(SortDirection::Asc) => {
                results.sort_by(|a, b| a.request.timestamp.cmp(&b.request.timestamp));
            }
            Some(SortDirection::Desc) => {
                results.sort_by(|a, b| b.request.timestamp.cmp(&a.request.timestamp));
            }
            None => {
                // Default to descending (newest first) if no sort direction specified
                results.sort_by(|a, b| b.request.timestamp.cmp(&a.request.timestamp));
            }
        }

        results
    }

    #[cfg(test)]
    pub async fn get_request_by_id(&self, id: &str) -> Option<RequestExchange> {
        let requests = self.requests.read().await;
        requests.get(id).cloned()
    }

    #[cfg(test)]
    pub async fn clear(&self) {
        let mut requests = self.requests.write().await;
        requests.clear();
    }

    #[cfg(test)]
    pub async fn get_count(&self) -> usize {
        let requests = self.requests.read().await;
        requests.len()
    }

    /// Returns a broadcast receiver that delivers each completed exchange (i.e.,
    /// one where a response has been stored).
    pub fn subscribe_requests(&self) -> broadcast::Receiver<RequestExchange> {
        self.request_sender.subscribe()
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
            response_time_ms: None,
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
    async fn test_get_next_pending_returns_id_and_timestamp() {
        let storage = RequestStorage::new(100);

        let request_time = Utc::now();
        let mut request = create_test_request("req1", "tunnel1", "GET", "http://example.com");
        request.timestamp = request_time;
        storage
            .store_request_with_connection(request, "conn1")
            .await;

        let result = storage
            .get_next_pending_request_for_connection("conn1")
            .await;
        assert!(result.is_some());
        let (id, timestamp) = result.unwrap();
        assert_eq!(id, "req1");
        assert_eq!(timestamp, request_time);
    }

    #[tokio::test]
    async fn test_get_next_pending_none_for_unknown_connection() {
        let storage = RequestStorage::new(100);
        assert!(
            storage
                .get_next_pending_request_for_connection("unknown")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn test_response_time_ms_stored_correctly() {
        let storage = RequestStorage::new(100);

        let request_time = Utc::now();
        let mut request = create_test_request("req1", "tunnel1", "GET", "http://example.com");
        request.timestamp = request_time;
        storage
            .store_request_with_connection(request, "conn1")
            .await;

        let (id, req_timestamp) = storage
            .get_next_pending_request_for_connection("conn1")
            .await
            .unwrap();
        let response_time = req_timestamp + chrono::Duration::milliseconds(42);
        let elapsed = response_time - req_timestamp;
        let mut response = create_test_response(&id, 200);
        response.timestamp = response_time;
        response.response_time_ms = Some(elapsed.num_microseconds().unwrap_or(0) as f64 / 1000.0);
        storage.store_response(response).await;

        let exchange = storage.get_request_by_id("req1").await.unwrap();
        let response_time_ms = exchange.response.unwrap().response_time_ms.unwrap();
        assert!((response_time_ms - 42.0).abs() < 1.0);
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

    #[tokio::test]
    async fn test_query_requests_sort_by_timestamp_asc() {
        let storage = RequestStorage::new(100);
        let base_time = Utc::now();

        // Create requests with different timestamps
        let req1 = StoredRequest {
            id: "req1".to_string(),
            timestamp: base_time,
            tunnel_name: "tunnel1".to_string(),
            method: "GET".to_string(),
            url: "http://example.com/1".to_string(),
            headers: HashMap::new(),
            body: vec![],
            raw_request: vec![],
        };

        let req2 = StoredRequest {
            id: "req2".to_string(),
            timestamp: base_time + chrono::Duration::minutes(1),
            tunnel_name: "tunnel1".to_string(),
            method: "GET".to_string(),
            url: "http://example.com/2".to_string(),
            headers: HashMap::new(),
            body: vec![],
            raw_request: vec![],
        };

        let req3 = StoredRequest {
            id: "req3".to_string(),
            timestamp: base_time + chrono::Duration::minutes(2),
            tunnel_name: "tunnel1".to_string(),
            method: "GET".to_string(),
            url: "http://example.com/3".to_string(),
            headers: HashMap::new(),
            body: vec![],
            raw_request: vec![],
        };

        // Store in reverse order to test sorting
        storage.store_request(req3).await;
        storage.store_request(req1).await;
        storage.store_request(req2).await;

        let filter = QueryFilter {
            sort_direction: Some(SortDirection::Asc),
            ..Default::default()
        };

        let results = storage.query_requests(&filter).await;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].request.id, "req1"); // Oldest first
        assert_eq!(results[1].request.id, "req2");
        assert_eq!(results[2].request.id, "req3"); // Newest last
    }

    #[tokio::test]
    async fn test_query_requests_sort_by_timestamp_desc() {
        let storage = RequestStorage::new(100);
        let base_time = Utc::now();

        // Create requests with different timestamps
        let req1 = StoredRequest {
            id: "req1".to_string(),
            timestamp: base_time,
            tunnel_name: "tunnel1".to_string(),
            method: "GET".to_string(),
            url: "http://example.com/1".to_string(),
            headers: HashMap::new(),
            body: vec![],
            raw_request: vec![],
        };

        let req2 = StoredRequest {
            id: "req2".to_string(),
            timestamp: base_time + chrono::Duration::minutes(1),
            tunnel_name: "tunnel1".to_string(),
            method: "GET".to_string(),
            url: "http://example.com/2".to_string(),
            headers: HashMap::new(),
            body: vec![],
            raw_request: vec![],
        };

        let req3 = StoredRequest {
            id: "req3".to_string(),
            timestamp: base_time + chrono::Duration::minutes(2),
            tunnel_name: "tunnel1".to_string(),
            method: "GET".to_string(),
            url: "http://example.com/3".to_string(),
            headers: HashMap::new(),
            body: vec![],
            raw_request: vec![],
        };

        // Store in chronological order
        storage.store_request(req1).await;
        storage.store_request(req2).await;
        storage.store_request(req3).await;

        let filter = QueryFilter {
            sort_direction: Some(SortDirection::Desc),
            ..Default::default()
        };

        let results = storage.query_requests(&filter).await;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].request.id, "req3"); // Newest first
        assert_eq!(results[1].request.id, "req2");
        assert_eq!(results[2].request.id, "req1"); // Oldest last
    }

    #[tokio::test]
    async fn test_query_requests_default_sort_desc() {
        let storage = RequestStorage::new(100);
        let base_time = Utc::now();

        // Create requests with different timestamps
        let req1 = StoredRequest {
            id: "req1".to_string(),
            timestamp: base_time,
            tunnel_name: "tunnel1".to_string(),
            method: "GET".to_string(),
            url: "http://example.com/1".to_string(),
            headers: HashMap::new(),
            body: vec![],
            raw_request: vec![],
        };

        let req2 = StoredRequest {
            id: "req2".to_string(),
            timestamp: base_time + chrono::Duration::minutes(1),
            tunnel_name: "tunnel1".to_string(),
            method: "GET".to_string(),
            url: "http://example.com/2".to_string(),
            headers: HashMap::new(),
            body: vec![],
            raw_request: vec![],
        };

        // Store in chronological order
        storage.store_request(req1).await;
        storage.store_request(req2).await;

        let filter = QueryFilter {
            sort_direction: None, // No sort direction specified
            ..Default::default()
        };

        let results = storage.query_requests(&filter).await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].request.id, "req2"); // Newest first (default behavior)
        assert_eq!(results[1].request.id, "req1"); // Oldest last
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
}
