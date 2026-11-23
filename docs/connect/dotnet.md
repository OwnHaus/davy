# Connect an ASP.NET Core app

Use `Sentry.AspNetCore`. Run `davy connect my-app --data DATA --sdk dotnet`
on the Davy host and export its DSN as `SENTRY_DSN` for the app process.
Keep it in an ignored `.env` loaded by your launcher or a systemd drop-in,
never a committed `appsettings.json`.

## 1. Install

<!-- e2e: skip, requires an ASP.NET Core application -->
```sh
dotnet add package Sentry.AspNetCore
```

The [official guide](https://docs.sentry.io/platforms/dotnet/guides/aspnetcore/)
currently documents SDK 7.0.1 for .NET 8 or later and directs .NET 6/7 apps
to SDK 4.x. This guide does not claim a .NET-to-Davy runtime test.

## 2. Start with the app

```csharp
var builder = WebApplication.CreateBuilder(args);
builder.WebHost.UseSentry(options =>
{
    options.Dsn = Environment.GetEnvironmentVariable("SENTRY_DSN") ?? "";
});
var app = builder.Build();
app.MapGet("/", () => "Hello");
app.Run();
```

Explicitly pass an empty string when the variable is unset. Issue #7
reported startup exceptions without it in an existing deployment. The
[current options docs](https://docs.sentry.io/platforms/dotnet/guides/aspnetcore/configuration/options/)
say an absent DSN disables sending; the explicit empty fallback also covers
that older deployment behavior. A nonempty invalid DSN still needs fixing.
A `.env` file alone does not export variables into a .NET process.

## 3. Check

Run the returned `test_event` command to check ingest without the SDK.
To verify the app too, temporarily capture after initialization:

```csharp
Sentry.SentrySdk.CaptureException(new InvalidOperationException("Davy test error"));
await Sentry.SentrySdk.FlushAsync(TimeSpan.FromSeconds(5));
```

Poll MCP `project_status` with `{"slug":"my-app"}` and check the event
count against its previous value. Use `issue_context` to read the stack.
