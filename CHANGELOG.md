v0.1.0
- Breaking: make `Client` non-generic, replace its public sender field with
  `Client::sender()`, and narrow low-level exports
- Add typed `serde` calls, notifications, and value/parameter helpers
- Add custom transports and managed server/client lifecycle controls
- Expose request IDs before responses and improve structured errors and disconnect handling

v0.0.6
- Quieter tracing

v0.0.5
- Add Client::join

v0.0.4
- Error::Connect variant to handle connection errors
- Fix a bug in concurrent request handling 
- Cleanups and refactorings
