# LambdaDoom project tasks. (The ldoom CLI has its own Makefile in rs-cli/.)

LAUNCH_BUCKET ?= lambdadoom-launch-932930471665
LAUNCH_REGION ?= us-east-1

.PHONY: sync-template

# Upload the CloudFormation template to the public Launch Stack bucket, so the
# README "Launch Stack" button always serves the current deploy/doom.yaml.
# Requires AWS credentials with s3:PutObject on the bucket.
sync-template:
	aws s3 cp deploy/doom.yaml s3://$(LAUNCH_BUCKET)/doom.yaml --region $(LAUNCH_REGION) --content-type text/yaml
	@echo "synced deploy/doom.yaml -> s3://$(LAUNCH_BUCKET)/doom.yaml"
