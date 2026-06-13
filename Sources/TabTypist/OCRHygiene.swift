import Foundation

// General-purpose hygiene for screen-OCR text headed into the completion prompt.
// Every rule here is STRUCTURAL:
// nothing pattern-matches a specific app's chrome, so the same pass cleans Telegram
// timestamps, Slack toolbars, terminal separators, and whatever ships next year.
//
// Why structure is enough: the things that poison a small completion model are not
// particular words but particular SHAPES — bare numbers (timestamps, view counts,
// ports), vowel-free glyph blobs (misreads of occluded text), symbol runs (box
// drawing, dividers), digits lodged inside lowercase words (`qu81ity`, the classic
// OCR letter→digit swap), and echoes of the user's own field text. A model fed
// "6:36 in the evening" between every message learns that timestamps are the local
// format and generates fake ones; strip the colon and the bare numbers die on shape
// alone, while "the meeting is at 5:30 tomorrow" survives because its words carry
// the line.
enum OCRHygiene {
    struct Line {
        let text: String
        let confidence: Float
    }

    /// Vision confidence below which a line is discarded outright — the recognizer's
    /// weakest guesses are the single largest source of garbage tokens.
    private static let minConfidence: Float = 0.4
    /// Maximum fraction of a line that may be unexpected symbols (not alphanumeric,
    /// space, or prose/code punctuation) before the line counts as glyph noise.
    private static let maxSymbolNoise = 0.2
    /// Minimum fraction of a line's non-space characters that must be alphanumeric;
    /// catches "allowed-punctuation" lines that still carry no words ("..88", "—— · ——").
    private static let minWordCharRatio = 0.5

    /// Punctuation that legitimately appears in prose, code, paths, and version
    /// strings — not counted as symbol noise.
    private static let prosePunctuation: Set<Character> = [
        ".", ",", "!", "?", ";", ":", "'", "\"", "(", ")", "[", "]", "{", "}",
        "-", "/", "&", "%", "$", "#", "@", "*", "+", "=", "<", ">", "`", "~",
        "_", "|", "\\", "\u{2019}", "\u{201C}", "\u{201D}",
    ]

    /// 1–2 character tokens that are real English words rather than OCR crumbs.
    private static let shortWords: Set<String> = [
        "a", "i", "an", "am", "as", "at", "be", "by", "do", "go", "he", "hi",
        "if", "in", "is", "it", "me", "my", "no", "of", "ok", "on", "or", "so",
        "to", "up", "us", "we",
    ]

    /// Vowel-free technical tokens worth keeping (the vowel rule would drop them).
    private static let technicalTokens: Set<String> = [
        "css", "dvd", "ftp", "gb", "ghz", "html", "http", "https", "kb", "mb",
        "npm", "php", "png", "ssh", "svg", "tb", "ts", "tv", "vs", "www", "xml",
    ]

    /// Runs every structural filter and returns the surviving, scrubbed lines in the
    /// order given. `fieldText` is the focused field's current contents (prefix +
    /// suffix); OCR re-reads it through the screenshot, and feeding it back as
    /// "context" only biases the model toward repeating the user.
    static func clean(_ lines: [Line], fieldText: String) -> [String] {
        let foldedField = foldWhitespace(fieldText.lowercased())
        return lines.compactMap { line in
            guard line.confidence >= minConfidence else { return nil }
            guard !line.text.contains("\u{FFFD}") else { return nil }
            guard symbolNoiseFraction(line.text) <= maxSymbolNoise else { return nil }
            guard !hasDigitSubstitutionToken(line.text) else { return nil }
            guard wordCharRatio(line.text) >= minWordCharRatio else { return nil }
            if isFieldEcho(line.text, foldedField: foldedField) { return nil }
            return scrubTokens(line.text)
        }
    }

    // MARK: – Line-level structure

    private static func symbolNoiseFraction(_ text: String) -> Double {
        let chars = Array(text)
        guard !chars.isEmpty else { return 0 }
        let noise = chars.lazy.filter { c in
            !(c == " " || c.isLetter || c.isNumber || prosePunctuation.contains(c))
        }.count
        return Double(noise) / Double(chars.count)
    }

    private static func wordCharRatio(_ text: String) -> Double {
        var nonSpace = 0, word = 0
        for c in text where !c.isWhitespace {
            nonSpace += 1
            if c.isLetter || c.isNumber { word += 1 }
        }
        guard nonSpace > 0 else { return 0 }
        return Double(word) / Double(nonSpace)
    }

    /// A digit with a lowercase letter before it and any letter after it, inside one
    /// token, is the OCR letter→digit misread shape (`qu81ity`, `h3llo`). Real tokens
    /// don't look like that: `utf8` and `v2` have nothing after the digit, `3D` and
    /// `5070` nothing lowercase before it, `RTX5070` only uppercase before it.
    private static func hasDigitSubstitutionToken(_ text: String) -> Bool {
        for token in text.split(whereSeparator: \.isWhitespace) {
            let chars = Array(token)
            for (i, c) in chars.enumerated() where c.isNumber {
                if chars[..<i].contains(where: \.isLowercase),
                   chars[(i + 1)...].contains(where: \.isLetter) {
                    return true
                }
            }
        }
        return false
    }

    private static func isFieldEcho(_ line: String, foldedField: String) -> Bool {
        guard !foldedField.isEmpty else { return false }
        let folded = foldWhitespace(line.lowercased())
        // Sub-4-char lines would match almost any field text coincidentally.
        return folded.count >= 4 && foldedField.contains(folded)
    }

    private static func foldWhitespace(_ s: String) -> String {
        s.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    // MARK: – Token-level scrub

    private enum TokenClass {
        case drop
        case weak    // kept, but can't carry a line by itself
        case strong  // kept, and proves the line has content
    }

    /// Replaces prompt-shaped punctuation with spaces (word boundaries survive;
    /// structural glue like the colon in "6:36" does not), classifies each token,
    /// and keeps the line only when at least half its tokens survive AND at least
    /// one of them is strong. A line that fails is UI chrome, not content.
    private static func scrubTokens(_ line: String) -> String? {
        let spaced = String(String.UnicodeScalarView(line.unicodeScalars.map { sc in
            if CharacterSet.alphanumerics.contains(sc) || CharacterSet.whitespaces.contains(sc)
                || sc == "@" || sc == "." || sc == "'" || sc == "\u{2019}" {
                return sc
            }
            return " "
        }))
        let tokens = spaced.split(whereSeparator: \.isWhitespace).map(String.init)
        guard !tokens.isEmpty else { return nil }

        var kept: [String] = []
        var hasStrong = false
        for token in tokens {
            switch classify(token) {
            case .drop: continue
            case .weak: kept.append(token)
            case .strong:
                kept.append(token)
                hasStrong = true
            }
        }
        guard hasStrong, kept.count * 2 >= tokens.count else { return nil }
        return kept.joined(separator: " ")
    }

    private static func classify(_ token: String) -> TokenClass {
        // Bare numbers: timestamps (post-strip), view counts, ports, phone fragments.
        if token.allSatisfy(\.isNumber) { return .drop }
        // Emails, domains, file names carry strong context.
        if isEmailOrDomainLike(token) { return .strong }
        // Letters outside ASCII (CJK, Cyrillic, Arabic, accented Latin, …): the
        // Latin-tuned rules below would strip non-English text to nothing.
        if token.unicodeScalars.contains(where: { $0.value > 127 && CharacterSet.letters.contains($0) }) {
            return .strong
        }
        let lower = token.lowercased()
        if token.count <= 2 {
            return shortWords.contains(lower) ? .weak : .drop
        }
        // Letter–digit blends ("WWDEV429", "1z8x"): identifiers, not prose.
        if token.contains(where: \.isLetter), token.contains(where: \.isNumber) {
            return .drop
        }
        // One glyph making up half a 4+ char token is recognizer stutter ("IIIIl").
        if isRepeatedGlyph(lower) { return .drop }
        if lower.contains(where: { "aeiouy".contains($0) }) { return .strong }
        return technicalTokens.contains(lower) ? .weak : .drop
    }

    private static func isEmailOrDomainLike(_ token: String) -> Bool {
        let at = token.split(separator: "@", omittingEmptySubsequences: false)
        if at.count == 2, at[0].contains(where: \.isLetter) {
            return isEmailOrDomainLike(String(at[1]))
        }
        let dots = token.split(separator: ".", omittingEmptySubsequences: false)
        return dots.count >= 2
            && dots.allSatisfy { !$0.isEmpty }
            && dots.contains { $0.contains(where: \.isLetter) }
    }

    private static func isRepeatedGlyph(_ lower: String) -> Bool {
        let scalars = lower.unicodeScalars.filter { CharacterSet.alphanumerics.contains($0) }
        guard scalars.count >= 4 else { return false }
        var counts: [UnicodeScalar: Int] = [:]
        for s in scalars { counts[s, default: 0] += 1 }
        return (counts.values.max() ?? 0) * 2 >= scalars.count
    }
}
