# Send logs over OpenTelemetry

Davy takes OTLP logs over HTTP, in protobuf or JSON. Point the exporter at it with two environment variables:

<!-- e2e: skip, e2e/run.sh runs the OpenTelemetry exporters -->
```sh
export OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=https://davy.example.com/v1/logs
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer $DAVY_LOG_KEY"
```

`DAVY_LOG_KEY` is the key in the project's DSN, the part between `://` and `@`. It is not a secret.

Davy serves only `/v1/logs`. Point the trace and metric exporters elsewhere or turn them off with `OTEL_TRACES_EXPORTER=none` and `OTEL_METRICS_EXPORTER=none`.

`service.name` becomes the record's service, and the other resource attributes are kept as `resource.*` attributes, so `attributes.resource.deployment.environment:test` finds them.
