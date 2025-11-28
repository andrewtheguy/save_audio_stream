group "default" {
  targets = ["amd64", "arm64"]
}

target "amd64" {
  dockerfile = "Dockerfile"
  platforms  = ["linux/amd64"]
  target     = "export"
  output     = ["type=local,dest=tmp/amd64"]
}

target "arm64" {
  dockerfile = "Dockerfile"
  platforms  = ["linux/arm64"]
  target     = "export"
  output     = ["type=local,dest=tmp/arm64"]
}
