# Owned Process author fixture

This stereo CLAP effect emits exactly one Begin(2), Value(3, 0.625), End(4)
batch from its actual DSP Process call. It has one automatable parameter and
no GUI or State extension. The first three PCM frames retain the incoming
gain; following frames use the independently declared 0.625 value. Later
calls consume the host's actual parameter input; they do not repeat the batch.

Build this fixture and the product from the same frozen source. The product
test must discover this exact binary and helper. This fixture supplies no
author, installed, Media or device proof by itself.
