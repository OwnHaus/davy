# The release image: the static musl binary from dist/ on distroless, as the
# nonroot user. The release workflow builds it after the binaries, so nothing
# compiles here and the image holds the same bytes as the downloads.
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
ARG TARGETARCH
COPY --chmod=0755 dist/image/${TARGETARCH}/davy /davy
# An empty /data owned by nonroot, so a new named volume is writable.
COPY --chown=65532:65532 dist/image/data /data
USER nonroot
VOLUME /data
EXPOSE 8000
ENTRYPOINT ["/davy"]
