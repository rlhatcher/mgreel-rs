// Reel name saves run one at a time, in the order they were made, so each
// one sends what the last one wrote. Anything that moves or replaces reels
// waits for them first, so a name can't land on a reel that arrived after it
// was typed. The queue outlives the Library, so leaving it doesn't drop a
// save another tab needs to wait for.

let last: Promise<void> = Promise.resolve();

/** Run `save` after every save queued before it. It reports its own errors. */
export function queueNameSave(save: () => Promise<void>) {
  last = last.then(save).catch((e) => console.error(e));
}

/** Settles once every save queued so far has finished. */
export function nameSaves(): Promise<void> {
  return last;
}
