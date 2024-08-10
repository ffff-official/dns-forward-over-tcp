# DNS Forward over TCP

As a dns server. forward data over TCP.

# How to use?

#### run server

```shell
./dns-forward-over-tcp -p 5353 -u 8.8.8.8:53
```

#### test 
```shell
dig @127.0.0.1 -p 5353 google.com
```

#### for developers
```rust
struct NoneCallback {}

#[async_trait]
impl RecordCallback<bool> for NoneCallback {
    async fn request(&self, res: &dns_parser::Packet<'_>) -> (bool, Option<bool>) {
        return (true, None);
    }

    async fn response(&self, req: Option<&dns_parser::Packet<'_>>, context: Option<bool>) {}
}

DnsServer::run(
        Some("53".to_string()),
        Some("8.8.8.8:53".to_string()),
        Some(4),
        Box::new(NoneCallback {}),
    )
    .await?;
```
