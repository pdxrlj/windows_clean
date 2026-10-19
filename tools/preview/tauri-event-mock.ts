/** Dev-only no-op mock of @tauri-apps/api/event for the browser preview. */

export type UnlistenFn = () => void;

export async function listen<T>(
  _event: string,
  _handler: (event: { payload: T }) => void
): Promise<UnlistenFn> {
  return () => {};
}

export async function once<T>(
  _event: string,
  _handler: (event: { payload: T }) => void
): Promise<UnlistenFn> {
  return () => {};
}

export async function emit(_event: string, _payload?: unknown): Promise<void> {}
