# Connect a Go app

Davy takes errors from the official Sentry SDK for Go. Only the DSN changes. Checked with `sentry-go` 0.50.

## 1. Install the SDK

<!-- e2e: skip, e2e covers the Node SDK -->
```sh
go get github.com/getsentry/sentry-go
```

## 2. Start it in main

```go
err := sentry.Init(sentry.ClientOptions{
	Dsn:     os.Getenv("SENTRY_DSN"),
	Release: os.Getenv("RELEASE"),
})
if err != nil {
	log.Fatalf("sentry: %v", err)
}
defer sentry.Flush(2 * time.Second)
```

Set `SENTRY_DSN` to the DSN from [install step 9](../install.md#9-connect-the-app). Report an error with `sentry.CaptureException(err)`. The SDK's middleware for `net/http`, Gin, Echo and the others works unchanged.

## 3. Check

Call `sentry.CaptureException(errors.New("Davy test error"))` once, then:

<!-- e2e: skip, e2e covers the Node SDK -->
```sh
sudo runuser -u davy -- davy issues --data /var/lib/davy --project my-app
```

It prints a line with `Davy test error`.
