import { invoke } from '@tauri-apps/api/core';

// Keep operations ordered across view unmount/remount. A new listening room
// must not read the previous snapshot while an older room is still saving.
let pending: Promise<unknown> = Promise.resolve();
function ordered<T>(operation: () => Promise<T>): Promise<T> {
  const result = pending.then(operation);
  pending = result.catch(() => undefined);
  return result;
}
export const readBackground = () => ordered(() => invoke<ArrayBuffer | number[]>('player_background'));
export const saveBackground = (bytes: number[]) => ordered(() => invoke<void>('set_player_background', {bytes}));
export const resetBackground = () => ordered(() => invoke<void>('clear_player_background'));
