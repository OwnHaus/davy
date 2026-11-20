# Connect a Python app

Davy takes errors from the official Sentry SDK for Python. Only the DSN changes. Checked with `sentry-sdk` 2.75.

## 1. Install the SDK

<!-- e2e: skip, e2e covers the Node SDK -->
```sh
pip install sentry-sdk
```

Use the project's tool if it is not pip (`uv add sentry-sdk`, `poetry add sentry-sdk`).

## 2. Start it with the app

As early as possible, before the web framework starts:

```python
import os
import sentry_sdk

sentry_sdk.init(
    dsn=os.environ["SENTRY_DSN"],
    release=os.environ.get("RELEASE"),
    environment=os.environ.get("ENVIRONMENT"),
)
```

Set `SENTRY_DSN` to the DSN from [install step 9](../install.md#9-connect-the-app). Django, Flask, FastAPI and Celery are picked up on their own.

## 3. Check

<!-- e2e: skip, e2e covers the Node SDK -->
```sh
python -c "
import os, sentry_sdk
sentry_sdk.init(dsn=os.environ['SENTRY_DSN'])
sentry_sdk.capture_exception(RuntimeError('Davy test error'))
sentry_sdk.flush(5)
"
sudo runuser -u davy -- davy issues --data /var/lib/davy --project my-app
```

The last command prints a line with `RuntimeError: Davy test error`.
