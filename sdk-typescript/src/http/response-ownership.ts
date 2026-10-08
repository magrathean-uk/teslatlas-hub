export async function cancelResponseBody(response: Response): Promise<void> {
  const body = response.body;
  if (body === null || body.locked) return;
  try {
    // Cleanup must not replace the original failure or wait on a custom sink.
    void body.cancel().catch(() => undefined);
  } catch {}
}

export async function withResponseOwnership<T>(
  response: Response,
  consume: () => Promise<T>,
): Promise<T> {
  try {
    return await consume();
  } catch (error) {
    await cancelResponseBody(response);
    throw error;
  }
}
