The helper's background delivery (Sources/MacHelper/WindowServer.swift, the background click and scroll in
Sources/MacHelper/Synth.swift, and Sources/MacKit/ClickPlan.swift) was written from Cua Driver's macOS input code,
whose sequence of focus records that gives a window key status without raising it comes from yabai.

- Cua Driver: https://github.com/trycua/cua (libs/cua-driver)
- yabai: https://github.com/koekeishiya/yabai

Both are distributed under the MIT License:

MIT License

Copyright (c) 2025 Cua AI, Inc.
Copyright (c) 2019 Åsmund Vikane

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
