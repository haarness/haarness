/**
 * How much taller than a line of text the lines holding images are: images
 * side by side share one line, so each line counts once, by its tallest.
 */
export function pictureLines(content: ParentNode, line: number) {
  return lineExtra(
    Array.from(content.querySelectorAll("img, .project-image"), (picture) => picture.getBoundingClientRect()),
    line,
  );
}

/** `pictureLines` for the boxes of the pictures: overlapping boxes share a line. */
export function lineExtra(pictures: readonly { top: number; bottom: number }[], line: number) {
  const boxes = pictures.map(({ top, bottom }) => ({ top, bottom })).sort((a, b) => a.top - b.top);
  let extra = 0;
  for (let index = 0; index < boxes.length; ) {
    let { top, bottom } = boxes[index++];
    while (index < boxes.length && boxes[index].top < bottom) bottom = Math.max(bottom, boxes[index++].bottom);
    extra += Math.max(0, bottom - top - line);
  }
  return extra;
}
