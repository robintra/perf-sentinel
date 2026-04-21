export default async function globalTeardown() {
  const server = globalThis.__psServer;
  if (!server) return;
  server.kill("SIGTERM");
  await new Promise((resolve) => {
    const killTimer = setTimeout(() => {
      try {
        server.kill("SIGKILL");
      } catch {
        /* already gone */
      }
      resolve(undefined);
    }, 2000);
    server.once("exit", () => {
      clearTimeout(killTimer);
      resolve(undefined);
    });
  });
}
