# Send logs from pino and other Logtail shippers

Davy takes logs in the Logtail (Better Stack) format. A shipper that sends to Better Stack sends to Davy when you change two settings:

- **Endpoint**: the public URL, such as `https://davy.example.com`.
- **Source token**: the key in the project's DSN, the part between `://` and `@`.

The key is not a secret. It only lets a client send logs to that project.

## pino

<!-- e2e: skip, e2e/run.sh ships logs with @logtail/pino -->
```sh
npm install pino @logtail/pino
```

```js
import pino from 'pino'

const transport = pino.transport({
  target: '@logtail/pino',
  options: {
    sourceToken: process.env.DAVY_LOG_KEY,
    options: { endpoint: 'https://davy.example.com' },
  },
})
export const log = pino(transport)
```

## Others

`@logtail/node`, Vector and Fluent Bit take the same two settings under their own names. A plain HTTP POST works too:

<!-- e2e: skip, e2e/run.sh posts JSON logs -->
```sh
curl -fsS -X POST https://davy.example.com/ -H "Authorization: Bearer $DAVY_LOG_KEY" \
  -H 'Content-Type: application/json' -d '{"message":"payment timed out","level":"error"}'
```

## Search

<!-- e2e: skip, e2e/run.sh searches logs -->
```sh
sudo runuser -u davy -- davy logs --data /var/lib/davy --project my-app --since 1h --level warn 'timeout'
```

Logs are kept 30 days unless `davy serve` gets `--log-retention-days`.
